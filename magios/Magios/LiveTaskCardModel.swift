import Foundation

/// The chat live task card's state: one card for a task's whole life, from
/// planning through approval to the run's end. Pure — no SwiftUI, no clock of
/// its own (the view passes `now`).
///
/// iOS port of `ui/unified-ui/src/lib/magician/chat/next/liveTask.ts` and the
/// state rules `LiveTaskCard.svelte` derives from it (timer, Cancel, live
/// activity, run list, plan preview, expandability). See
/// `docs/archive/plans/2026-10-01-mobile-lively-chat-and-task-detail-design.md` §2.

enum LiveTaskStatus: String, Equatable {
    /// Not started: `ready` (plan approved), `pending` (never planned) or
    /// `rejected`. No run exists, so the card does not tick, offer Cancel or
    /// read as working.
    case queued
    case running
    case waiting
    case completed
    case partial
    case failed
    case cancelled
}

/// One run's state, the vocabulary of the turn-progress rows.
enum LiveTaskStepStatus: String, Equatable {
    case running, done, failed, waiting
}

struct LiveTaskStep: Equatable {
    let key: String
    let label: String
    let status: LiveTaskStepStatus
}

/// One run's output blocks, as the card shows them.
struct LiveTaskOutput: Equatable {
    let key: String
    /// Run label, only when the task has more than one run.
    let label: String?
    let sessionId: String?
    let count: Int
}

/// One run of the task (web `ChatRenderTaskExecutionGroup`): its latest
/// update's status and ids, every update's time and the latest update's output
/// count.
struct LiveTaskRunInput: Equatable {
    var id: String
    var status: String?
    var executionId: String?
    var sessionId: String?
    /// Epoch milliseconds, or epoch seconds (below 1e11).
    var timestamps: [Double] = []
    var outputCount: Int = 0
}

/// What the card reads: the task's latest `task_status_update` content, the
/// task store's status when the host has it, and the task's runs.
struct LiveTaskCardInput: Equatable {
    var messageId: String
    var taskId: String?
    var displayLabel: String?
    /// The card message's status.
    var status: String?
    /// The task's status in the task store, when the host has it.
    var storeStatus: String?
    var summary: String?
    var synthesisPending: Bool = false
    /// `full` or `partial`.
    var completionKind: String?
    var executionId: String?
    /// Epoch milliseconds, or epoch seconds (below 1e11).
    var createdAt: Double?
    /// Output blocks on the message itself (carried into a run's input by the
    /// host; the card counts outputs per run, as the web does).
    var outputCount: Int = 0
    var runs: [LiveTaskRunInput] = []
    /// The task record's created and latest updated times
    /// (`TaskStoreRecord`), when the host fetched them.
    var recordCreatedAt: Date? = nil
    var recordUpdatedAt: Date? = nil
}

extension LiveTaskCardInput {
    /// From the chat's `TaskStatusModel`. The chat falls back to the raw task
    /// id as its title; the card's title rule never shows one, so an id-shaped
    /// title is passed as no label.
    init(
        task: TaskStatusModel,
        messageId: String,
        storeStatus: String? = nil,
        createdAt: Double? = nil,
        completionKind: String? = nil,
        runs: [LiveTaskRunInput] = []
    ) {
        let taskId = task.taskId == "unknown-task" ? nil : task.taskId
        self.init(
            messageId: messageId,
            taskId: taskId,
            displayLabel: task.title,
            status: task.status,
            storeStatus: storeStatus,
            summary: task.summary,
            synthesisPending: task.synthesisPending,
            completionKind: completionKind,
            executionId: task.executionId,
            createdAt: createdAt,
            outputCount: task.outputFiles.count,
            runs: runs
        )
    }
}

struct LiveTaskCardState: Equatable {
    let id: String
    /// The real task id (absent for malformed status messages).
    let taskId: String?
    /// Latest run's execution id, for Inspect.
    let executionId: String?
    let title: String
    let status: LiveTaskStatus
    /// The human status word ("Running", "Plan ready for review", …);
    /// "Partially completed" for a partial success.
    let verb: String
    let terminal: Bool
    /// Plan approved and not started: the card can offer Run.
    let startable: Bool
    /// One step per run, in order.
    let steps: [LiveTaskStep]
    /// Non-terminal only, and only when the reason does not already say it.
    let currentStep: String?
    let startedAt: Date?
    let finishedAt: Date?
    /// The card's "Created": the task record's created time when known, else
    /// the card's first update.
    let createdAt: Date?
    /// The card's "Updated": the later of the task record's update time and
    /// the card's latest update.
    let updatedAt: Date?
    /// Why the task is where it is (markdown): the trimmed `summary`. Dropped
    /// when the store's status replaced the message's, since the summary then
    /// describes a state the task left.
    let reason: String?
    /// Per-run output counts, runs without outputs omitted.
    let outputs: [LiveTaskOutput]

    /// Outputs a collapsed card shows before "+N more".
    static let outputPreviewLimit = 2

    init(_ input: LiveTaskCardInput) {
        let synthesisPending = input.synthesisPending
        let contentStatus = Self.effectiveStatus(message: input.status, store: input.storeStatus)
        let partial = !synthesisPending
            && Self.isPartialCompletion(contentStatus, completionKind: input.completionKind)
        // A synthesizing task stays live even if its status already reads terminal.
        let terminal = !synthesisPending && (partial || Self.isTerminal(contentStatus))
        let status: LiveTaskStatus = synthesisPending
            ? .running
            : partial ? .partial : Self.liveStatus(contentStatus)
        let visualVerb = Self.statusVerb(contentStatus, synthesisPending: synthesisPending)
        let verb = partial ? "Partially completed" : visualVerb
        let reason = contentStatus == input.status ? Self.text(input.summary) : nil

        let total = input.runs.count
        let steps = input.runs.enumerated().map { index, run in
            LiveTaskStep(
                key: run.id,
                label: Self.runLabel(run, index: index, total: total),
                status: Self.stepStatus(run.status)
            )
        }

        // No progress fraction: runs are not a plan, so done ÷ runs is not a
        // meaningful percentage.
        var currentStep: String?
        if !terminal {
            let running = steps.last { $0.status == .running }
            if synthesisPending {
                currentStep = visualVerb
            } else if reason != nil {
                // The reason already says what is happening; with several runs,
                // still name the live one.
                currentStep = total > 1 ? running?.label : nil
            } else {
                currentStep = running?.label ?? visualVerb
            }
        }

        let times = ([input.createdAt] + input.runs.flatMap { $0.timestamps.map(Optional.some) })
            .compactMap { Self.epochSeconds($0) }

        let outputs = input.runs.enumerated().compactMap { index, run -> LiveTaskOutput? in
            guard run.outputCount > 0 else { return nil }
            return LiveTaskOutput(
                key: run.id,
                label: total > 1 ? Self.runLabel(run, index: index, total: total) : nil,
                sessionId: run.sessionId,
                count: run.outputCount
            )
        }
        let latestRunExecution = Self.text(input.runs.last?.executionId)
        let taskId = Self.text(input.taskId)

        self.id = taskId ?? input.messageId
        self.taskId = taskId
        self.executionId = latestRunExecution ?? Self.text(input.executionId)
        self.title = TaskTitleRules.labelOrShortIdTitle(label: input.displayLabel, id: taskId)
        self.status = status
        self.verb = verb
        self.terminal = terminal
        self.startable = !synthesisPending && contentStatus == "ready"
        self.steps = steps
        self.currentStep = currentStep
        let firstUpdate = times.min().map { Date(timeIntervalSince1970: $0) }
        let latestUpdate = times.max().map { Date(timeIntervalSince1970: $0) }
        self.startedAt = firstUpdate
        self.finishedAt = terminal ? latestUpdate : nil
        self.createdAt = input.recordCreatedAt ?? firstUpdate
        self.updatedAt = [input.recordUpdatedAt, latestUpdate].compactMap { $0 }.max()
        self.reason = reason
        self.outputs = outputs
    }

    // MARK: - Derived card rules (LiveTaskCard.svelte)

    /// A task that has not started has no run to time, stop or stream.
    var notStarted: Bool { status == .queued }

    /// The card's headline word. The same as `verb`; named for the view.
    var headline: String { verb }

    /// The quiet tag first in the meta row of an unstarted card.
    var tagText: String? { notStarted ? "Not started" : nil }

    /// "Needs you" beside the status of a task waiting on the reader.
    var needsYou: Bool { status == .waiting }

    /// Elapsed time is shown (ticking or, once terminal, frozen).
    var showsTimer: Bool { startedAt != nil && !notStarted }

    /// The elapsed time ticks: live, started, not unstarted.
    var ticks: Bool { showsTimer && !terminal }

    /// Cancel is offered (the view also needs a handler).
    var canCancel: Bool { !terminal && !notStarted }

    /// The live activity (the task-scoped turn progress) streams only while
    /// the task runs and has a real id.
    var showsLiveActivity: Bool { !terminal && !notStarted && taskId != nil }

    /// Not started or waiting for review: the plan is what the card previews,
    /// and the host fetches it once when the card has no steps yet.
    var wantsPlan: Bool { !terminal && (notStarted || status == .waiting) && taskId != nil }

    func showsPlanPreview(planStepCount: Int) -> Bool { wantsPlan && planStepCount > 0 }

    /// `m:ss` / `h:mm:ss` from start to the end (terminal) or `now`; nil when
    /// the card shows no time.
    func elapsedText(now: Date) -> String? {
        guard showsTimer, let startedAt else { return nil }
        return Self.formatElapsed((finishedAt ?? now).timeIntervalSince(startedAt))
    }

    /// The run list only when it says something: while running (the fallback
    /// when there is no live activity), or a finished task whose runs ended
    /// differently. Never a lone "Run · done".
    func showsRuns(liveActivityAvailable: Bool) -> Bool {
        guard !liveActivityAvailable, !steps.isEmpty else { return false }
        let loneDoneRun = steps.count == 1 && steps[0].status == .done
        if loneDoneRun { return false }
        let distinct = Set(steps.map(\.status)).count
        return !terminal || (steps.count >= 2 && distinct >= 2)
    }

    /// A finished task's outputs are the answer: they sit in the card body. A
    /// running task's partial outputs stay in the details.
    var outputsInBody: Bool { terminal && !outputs.isEmpty }

    var totalOutputCount: Int { outputs.reduce(0) { $0 + $1.count } }

    func hiddenOutputCount(expanded: Bool) -> Int {
        expanded ? 0 : max(0, totalOutputCount - Self.outputPreviewLimit)
    }

    /// Expandable only when expanding reveals something. Cancel is not part of
    /// it: it lives in the title row.
    static func isExpandable(
        hasSteps: Bool,
        hasRuns: Bool,
        hasOutputs: Bool,
        hasClampedReason: Bool,
        hasHiddenOutputs: Bool,
        hasPlanPreview: Bool
    ) -> Bool {
        hasSteps || hasRuns || hasOutputs || hasClampedReason || hasHiddenOutputs || hasPlanPreview
    }

    /// The card's rule over its own state. `activityHasContent`: the live
    /// activity rendered something. `liveActivityAvailable` defaults to
    /// `showsLiveActivity` (the host renders the activity whenever it may).
    /// Once open it stays a toggle so it can close (`expanded`).
    func isExpandable(
        activityHasContent: Bool,
        planStepCount: Int,
        reasonOverflows: Bool,
        expanded: Bool = false,
        liveActivityAvailable: Bool? = nil
    ) -> Bool {
        let activity = liveActivityAvailable ?? showsLiveActivity
        return expanded || Self.isExpandable(
            hasSteps: activity && activityHasContent,
            hasRuns: showsRuns(liveActivityAvailable: activity),
            hasOutputs: !outputsInBody && !outputs.isEmpty,
            hasClampedReason: reasonOverflows,
            hasHiddenOutputs: hiddenOutputCount(expanded: expanded) > 0,
            hasPlanPreview: showsPlanPreview(planStepCount: planStepCount)
        )
    }

    /// `m:ss`, or `h:mm:ss` past an hour.
    static func formatElapsed(_ seconds: TimeInterval) -> String {
        let total = seconds.isFinite ? max(0, Int(seconds.rounded(.down))) : 0
        let h = total / 3600
        let m = (total % 3600) / 60
        let s = total % 60
        let ss = s < 10 ? "0\(s)" : "\(s)"
        if h > 0 { return "\(h):\(m < 10 ? "0\(m)" : "\(m)"):\(ss)" }
        return "\(m):\(ss)"
    }

    // MARK: - Status rules (liveTask.ts / taskStatus.ts)

    /// `archived` is final too: a card whose last status update never
    /// arrived must settle on it instead of reading "Running" forever.
    static func isTerminal(_ status: String?) -> Bool {
        status == "completed" || status == "failed" || status == "cancelled" || status == "archived"
    }

    /// `draft`: a plan waiting for the reader's review.
    static func isWaiting(_ status: String?) -> Bool {
        guard let status else { return false }
        return status == "paused" || status == "draft"
            || status.hasPrefix("waiting") || status.hasPrefix("awaiting")
    }

    static func isNotStarted(_ status: String?) -> Bool {
        status == "ready" || status == "pending" || status == "rejected"
    }

    static func isPartialCompletion(_ status: String?, completionKind: String?) -> Bool {
        status == "partial_success" || (status == "completed" && completionKind == "partial")
    }

    /// The card's latest message, unless the task store knows the task is not
    /// started while the message still says planning or draft.
    static func effectiveStatus(message: String?, store: String?) -> String? {
        let behind = message == nil || message == "planning" || message == "draft"
        return behind && isNotStarted(store) ? store : message
    }

    static func liveStatus(_ status: String?) -> LiveTaskStatus {
        switch status {
        case "completed": return .completed
        case "failed": return .failed
        case "cancelled": return .cancelled
        // Settled but not a failure: muted, like a cancelled card.
        case "archived": return .cancelled
        default:
            if isWaiting(status) { return .waiting }
            if isNotStarted(status) { return .queued }
            return .running
        }
    }

    static func stepStatus(_ status: String?) -> LiveTaskStepStatus {
        if status == "completed" || status == "partial_success" { return .done }
        if status == "failed" || status == "cancelled" { return .failed }
        if isWaiting(status) { return .waiting }
        return .running
    }

    /// The status word (web `taskStatusVisual(...).verb`).
    static func statusVerb(_ status: String?, synthesisPending: Bool) -> String {
        if synthesisPending { return "Preparing final result" }
        switch status {
        case "created": return "Created"
        case "planning": return "Planning"
        case "ready": return "Plan approved, not started"
        case "pending": return "Not started"
        case "draft": return "Plan ready for review"
        case "rejected": return "Plan rejected"
        case "running", "in_progress": return "Running"
        case "paused": return "Paused"
        case "completed": return "Completed"
        case "failed": return "Failed"
        case "cancelled": return "Cancelled"
        case "archived": return "Archived"
        default: return status ?? "Updated"
        }
    }

    /// Web `taskExecutionLabel`.
    static func runLabel(_ run: LiveTaskRunInput, index: Int, total: Int) -> String {
        guard text(run.executionId) != nil else { return total > 1 ? "Primary run" : "Run" }
        return total > 1 ? "Run \(index + 1)" : "Run"
    }

    /// Epoch seconds; values below 1e11 are already seconds, larger ones are ms.
    static func epochSeconds(_ value: Double?) -> TimeInterval? {
        guard let value, value.isFinite, value > 0 else { return nil }
        return value < 1e11 ? value : value / 1_000
    }

    private static func text(_ raw: String?) -> String? {
        guard let trimmed = raw?.trimmingCharacters(in: .whitespacesAndNewlines), !trimmed.isEmpty else {
            return nil
        }
        return trimmed
    }
}

// MARK: - Title rule

/// The drawer's title rule, as the live card uses it. iOS port of
/// `labelOrShortIdTitle` and what it needs from
/// `ui/unified-ui/src/lib/magician/tasks/taskPanelTitle.ts`.
enum TaskTitleRules {
    static let titleMaxChars = 90

    private static func regex(_ pattern: String, _ options: NSRegularExpression.Options = []) -> NSRegularExpression {
        // Literal patterns: a failure is a programming error caught by the tests.
        try! NSRegularExpression(pattern: pattern, options: options)
    }

    /// `<agent-slug> / ` at the very start of a prompt: the routing prefix.
    private static let routingPrefix = regex(#"^[a-z0-9]+(?:[-_][a-z0-9]+)*\s+/\s+"#)
    /// A leading meta sentence about *how* to run, not *what* to do.
    private static let metaSentence = regex(#"^(?:do\s+not|don['’]t|never)\b[^.!?\n]*[.!?]?\s*"#, .caseInsensitive)
    /// "Use (the) browser … to <goal>" keeps the goal; a bare "Use the browser." goes.
    private static let browserLead = regex(#"^use\s+(?:the\s+)?browser\b[^.!?\n]*?\bto\s+"#, .caseInsensitive)
    private static let browserSentence = regex(#"^use\s+(?:the\s+)?browser\b(?:(?!\bto\s)[^.!?\n])*[.!?]\s*"#, .caseInsensitive)
    /// Words a surface prefixes onto an id to make a title out of it.
    private static let idPrefix = regex(#"^(?:run|task|execution|job)\s+"#, .caseInsensitive)
    private static let shortIdPrefix = regex(#"^[a-z]+[_-](?=[a-z0-9])"#, .caseInsensitive)

    private static func replacing(_ regex: NSRegularExpression, in text: String, with template: String = "") -> String {
        regex.stringByReplacingMatches(
            in: text, range: NSRange(text.startIndex..., in: text), withTemplate: template
        )
    }

    private static func matches(_ regex: NSRegularExpression, _ text: String) -> Bool {
        regex.firstMatch(in: text, range: NSRange(text.startIndex..., in: text)) != nil
    }

    private static func trimmed(_ text: String?) -> String {
        (text ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// Whether `text` is an identifier rather than prose.
    static func looksLikeId(_ text: String?, id: String?) -> Bool {
        let value = trimmed(text)
        if value.isEmpty { return false }
        let ownId = trimmed(id)
        if !ownId.isEmpty, value.contains(ownId) { return true }
        let token = replacing(idPrefix, in: value)
        if token.rangeOfCharacter(from: .whitespacesAndNewlines) != nil { return false }
        return token.count >= 8 && token.rangeOfCharacter(from: .decimalDigits) != nil
    }

    static func isRoutedPrompt(_ text: String?) -> Bool {
        matches(routingPrefix, trimmed(text))
    }

    private static func stripDirectives(_ text: String) -> String {
        var rest = replacing(routingPrefix, in: trimmed(text))
        while true {
            let next = replacing(browserSentence, in: replacing(metaSentence, in: rest))
            if next == rest { break }
            rest = next
        }
        return trimmed(rest)
    }

    private static func capAtWord(_ text: String, max: Int) -> String {
        guard text.count > max else { return text }
        let cut = String(text.prefix(max))
        if let space = cut.lastIndex(of: " "), space > cut.startIndex {
            return String(cut[..<space]).trimmingCharacters(in: .whitespaces) + "…"
        }
        return cut.trimmingCharacters(in: .whitespaces) + "…"
    }

    /// A human title from a prompt, or nil when no prose is left.
    static func titleFromPrompt(_ prompt: String?) -> String? {
        guard let prompt, !prompt.isEmpty else { return nil }
        var rest = stripDirectives(prompt)
        if matches(browserLead, rest) {
            let goal = replacing(browserLead, in: rest)
            rest = goal.prefix(1).uppercased() + goal.dropFirst()
        }
        let firstLine = rest.components(separatedBy: "\n").first { !trimmed($0).isEmpty } ?? ""
        let firstSentence = replacing(regex(#"(?<=[.!?])\s+"#), in: trimmed(firstLine), with: "\u{0}")
            .components(separatedBy: "\u{0}").first ?? ""
        let sentence = trimmed(
            replacing(regex(#"\s+"#), in: replacing(regex(#"[.!?…]+$"#), in: firstSentence), with: " ")
        )
        if sentence.isEmpty || looksLikeId(sentence, id: nil) { return nil }
        return capAtWord(sentence, max: titleMaxChars)
    }

    /// The drawer heading from a label and id alone (web `deriveTaskTitle`
    /// with no title or description).
    static func deriveTitle(label: String?, id: String?) -> String? {
        let shownLabel = trimmed(label)
        if !shownLabel.isEmpty, !looksLikeId(shownLabel, id: id) {
            if !isRoutedPrompt(shownLabel) { return shownLabel }
            if let fromLabel = titleFromPrompt(shownLabel) { return fromLabel }
        }
        return nil
    }

    /// `task_2679cefd6f…` → `2679ce`.
    static func shortTaskId(_ id: String?) -> String? {
        let value = trimmed(id)
        if value.isEmpty { return nil }
        return String(replacing(shortIdPrefix, in: value).prefix(6))
    }

    /// The label through the title rules, else `Task · <short id>`, else
    /// `Task`. Never the raw id.
    static func labelOrShortIdTitle(label: String?, id: String?) -> String {
        if let derived = deriveTitle(label: label, id: id), !trimmed(derived).isEmpty { return derived }
        if let short = shortTaskId(id) { return "Task · \(short)" }
        return "Task"
    }
}
