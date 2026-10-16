import Foundation

/// The task detail screen's act rules: which acts it has, which opens, the
/// order they render in, and the one line each carries while closed. Pure.
///
/// iOS port of `actOrder` (`ui/unified-ui/src/lib/magician/tasks/taskCapabilities.ts`),
/// `actSummaries.ts` and `outputKindOf` (`taskPanelModel.ts`). `deriveActs` and
/// `defaultOpenAct` were already ported in `TaskCapabilities.swift`; `TaskActs`
/// re-exposes them so a view reads every act rule from one place.

enum TaskActs {
    static func deriveActs(_ caps: ActCapabilities) -> [ActId] {
        TaskCapabilities.deriveActs(caps)
    }

    static func defaultOpenAct(state: VerdictState, acts: [ActId], attention: HitlSource?) -> ActId? {
        TaskCapabilities.defaultOpenAct(state: state, acts: acts, attention: attention)
    }

    /// The act that leads the column, or nil to keep lifecycle order. **Result
    /// first**: a finished task's reader came for its output. A live or
    /// stopped-short run leads with Run. Waiting and queued keep lifecycle
    /// order: the ask, or the plan waiting to start, is in the act that
    /// already leads. No `default`, so a new state is a compile error here.
    static func lead(for state: VerdictState) -> ActId? {
        switch state {
        case .waiting, .queued: return nil
        case .failed, .stalled, .running, .paused, .cancelled: return .run
        case .archived, .finished: return .output
        }
    }

    /// `acts` (lifecycle order) with the state's leading act moved to the
    /// front, the rest in lifecycle order. A permutation of its input.
    static func actOrder(state: VerdictState, acts: [ActId]) -> [ActId] {
        guard let lead = lead(for: state), acts.contains(lead) else { return acts }
        return [lead] + acts.filter { $0 != lead }
    }
}

// MARK: - Act summaries

/// The plan's status (web `TaskPlanStatus`).
enum TaskPlanStatus: String, CaseIterable, Equatable {
    case planning, eliciting, draft, approved, rejected, failed

    /// Tolerant wire decode: an unknown status is nil (the summary then says nothing).
    init?(wire: String?) {
        guard let raw = wire?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased(),
              let status = TaskPlanStatus(rawValue: raw) else { return nil }
        self = status
    }

    /// In the reader's words. `planning` and `eliciting` are rewritten because
    /// neither word tells a reader anything.
    var copy: String {
        switch self {
        case .planning: return "being planned"
        case .eliciting: return "gathering what it needs before planning"
        case .draft: return "draft, not yet approved"
        case .approved: return "approved"
        case .rejected: return "rejected, so it will not run"
        case .failed: return "planning failed"
        }
    }
}

struct PlanSummaryInput: Equatable {
    /// nil when the task has a plan act but no status the client recognises.
    var status: TaskPlanStatus?
    var approvedAt: Date?
    /// The asks blocking the plan, in the order the act body lists them.
    var questions: [String]
    var now: Date
}

struct RunSummaryInput: Equatable {
    /// Steps recorded so far.
    var steps: Int
    var retries: Int
    /// Seconds; nil, negative or non-finite render no duration.
    var elapsed: TimeInterval?
    /// 1-based; non-nil while the run is live, swapping the count for the position.
    var currentStep: Int?
    var totalSteps: Int?
    /// Timeline entries recorded: a count under its own noun.
    var events: Int
}

/// What kind of file, for the output summary's plural.
enum OutputKind: String, Equatable {
    case document, image, other

    var plural: String {
        switch self {
        case .document: return "documents"
        case .image: return "images"
        case .other: return "other files"
        }
    }
}

struct ActOutputFile: Equatable {
    let name: String
    let kind: OutputKind
}

enum ActSummaries {
    /// A waiting question outranks the approval; an approved plan says when;
    /// an unknown status says nothing.
    static func planSummary(_ input: PlanSummaryInput) -> String {
        if !input.questions.isEmpty {
            let count = input.questions.count
            let waiting = "\(count) \(count == 1 ? "question" : "questions") waiting"
            let asked = input.questions[0].trimmingCharacters(in: .whitespacesAndNewlines)
            return asked.isEmpty ? waiting : "\(waiting) — \"\(asked)\""
        }
        if input.status == .approved, let when = approvalTime(input.approvedAt, now: input.now) {
            return "approved \(when)"
        }
        return input.status?.copy ?? ""
    }

    /// Live, the position replaces the step count; retries, events and the
    /// duration still apply. Empty segments drop out; nothing at all is
    /// `not started`.
    static func runSummary(_ input: RunSummaryInput) -> String {
        let progress: String? = input.currentStep == nil
            ? count(input.steps, "step", "steps")
            : TaskVerdict.stepPhrase(step: input.currentStep, total: input.totalSteps)
        let line = [
            progress,
            count(input.retries, "retry", "retries"),
            count(input.events, "event", "events"),
            TaskVerdict.durationIfKnown(input.elapsed)
        ]
        .compactMap { $0 }
        .filter { !$0.isEmpty }
        .joined(separator: " · ")
        return line.isEmpty ? "not started" : line
    }

    /// `report.md`, `report.md and notes.md`, `report.md and 2 images`.
    static func outputSummary(_ files: [ActOutputFile]) -> String {
        guard let first = files.first else { return "no output" }
        if files.count == 1 { return first.name }
        if files.count == 2 { return "\(first.name) and \(files[1].name)" }
        let rest = files.dropFirst()
        let kinds = Set(rest.map(\.kind))
        let plural = kinds.count == 1 ? rest.first!.kind.plural : OutputKind.other.plural
        return "\(first.name) and \(rest.count) \(plural)"
    }

    /// The Output act's line over task deliverables, by basename and kind.
    static func outputSummary(artifacts: [TaskDetailArtifact]) -> String {
        outputSummary(artifacts.map {
            ActOutputFile(name: $0.displayName, kind: outputKind(mediaType: $0.mediaType, path: $0.relativePath))
        })
    }

    private static let documentMediaTypes: Set<String> = [
        "application/pdf", "application/msword", "application/rtf",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
    ]
    private static let documentExtensions: Set<String> = ["md", "markdown", "txt", "pdf", "html", "htm", "doc", "docx", "rtf"]
    private static let imageExtensions: Set<String> = ["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "avif"]

    /// The mime is authoritative when present; the extension is the fallback.
    static func outputKind(mediaType: String?, path: String) -> OutputKind {
        let mime = (mediaType ?? "").trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        if !mime.isEmpty {
            if mime.hasPrefix("image/") { return .image }
            if mime.hasPrefix("text/") || documentMediaTypes.contains(mime) { return .document }
            return .other
        }
        let base = (path as NSString).lastPathComponent
        guard let dot = base.lastIndex(of: "."), dot > base.startIndex else { return .other }
        let ext = base[base.index(after: dot)...].lowercased()
        if imageExtensions.contains(ext) { return .image }
        if documentExtensions.contains(ext) { return .document }
        return .other
    }

    /// Web `formatRelativeTime`: `now` under a minute (a future instant
    /// clamps to it), then `Nm`, `Nh`, `Nd`, and a short date after a week.
    static func compactRelativeTime(_ date: Date?, now: Date) -> String {
        guard let date, date.timeIntervalSince1970.isFinite else { return "" }
        let delta = max(0, now.timeIntervalSince(date))
        if delta < 60 { return "now" }
        if delta < 3_600 { return "\(Int(delta / 60))m" }
        if delta < 86_400 { return "\(Int(delta / 3_600))h" }
        if delta < 7 * 86_400 { return "\(Int(delta / 86_400))d" }
        let formatter = DateFormatter()
        formatter.setLocalizedDateFormatFromTemplate("MMMd")
        return formatter.string(from: date)
    }

    /// When the plan was approved, phrased to follow the word `approved`.
    private static func approvalTime(_ approvedAt: Date?, now: Date) -> String? {
        let relative = compactRelativeTime(approvedAt, now: now)
        if relative.isEmpty { return nil }
        if relative == "now" { return "just now" }
        return relative.range(of: "^\\d+[mhd]$", options: .regularExpression) != nil
            ? "\(relative) ago" : "on \(relative)"
    }

    /// nil rather than `0 steps`, so a segment with nothing to report drops out.
    private static func count(_ n: Int, _ singular: String, _ plural: String) -> String? {
        n <= 0 ? nil : "\(n) \(n == 1 ? singular : plural)"
    }
}
