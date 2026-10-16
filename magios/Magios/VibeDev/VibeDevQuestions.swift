import SwiftUI

// What needs the owner, as cards with their options, and the code that acts
// on an option. Crew questions are derived from the crew record the server
// keeps (the same request ids Attention shows), so a phone answers a crew
// question exactly as the web cockpit does.

enum VibeDevOptionStyle: Equatable { case primary, plain, danger }

enum VibeDevQuestionAction: Equatable {
    case startCrew
    case dontStartCrew
    case keepLane(index: Int, lane: String)
    case keepNone
    case resolveWithAgent
    case resolveMyself
    case openCrewPullRequest
    case notNow
    case reviewRun
    case approveRun
}

struct VibeDevQuestionOption: Identifiable, Equatable {
    let label: String
    let style: VibeDevOptionStyle
    let action: VibeDevQuestionAction
    var id: String { label }
}

struct VibeDevNeedsYouItem: Identifiable, Equatable {
    enum Kind: Equatable { case crewStart, crewChoose, crewResolve, crewPullRequest, run }

    let id: String
    let kind: Kind
    let eyebrow: String
    let title: String
    let body: String
    let projectID: String
    let crew: VibeDevCrew?
    let run: VibeDevLiveRun?
    let options: [VibeDevQuestionOption]
    let updatedAtMs: Int64
}

enum VibeDevQuestions {
    /// The question a crew is waiting on, if any.
    static func question(for crew: VibeDevCrew, projectID: String, projectName: String) -> VibeDevNeedsYouItem? {
        let prefix = (crew.mode == .race ? "VibeDev race" : "VibeDev crew") + (projectName.isEmpty ? "" : " · \(projectName)")
        func item(_ kind: VibeDevNeedsYouItem.Kind, _ suffix: String, _ title: String, _ body: String,
                  _ options: [VibeDevQuestionOption]) -> VibeDevNeedsYouItem {
            VibeDevNeedsYouItem(id: "crew:\(crew.crewId):\(suffix)", kind: kind, eyebrow: prefix, title: title,
                                body: body, projectID: projectID, crew: crew, run: nil, options: options,
                                updatedAtMs: crew.updatedAtMs)
        }
        switch crew.status {
        case .proposed:
            return item(.crewStart, "start", "Start this crew?", crew.goal, [
                VibeDevQuestionOption(label: "Start", style: .primary, action: .startCrew),
                VibeDevQuestionOption(label: "Don't start", style: .plain, action: .dontStartCrew),
            ])
        case .choosing:
            var options: [VibeDevQuestionOption] = []
            for (index, lane) in crew.lanes.enumerated() where lane.role == .work && lane.status == .done {
                options.append(VibeDevQuestionOption(
                    label: "Keep \(lane.lane)", style: options.isEmpty ? .primary : .plain,
                    action: .keepLane(index: index, lane: lane.lane)))
            }
            options.append(VibeDevQuestionOption(label: "Keep none", style: .danger, action: .keepNone))
            return item(.crewChoose, "choose", "Which attempt do you keep?", crew.goal, options)
        case .conflict where crew.resolveRequestId != nil:
            let detail = crew.conflict.map { "\($0.lane) conflicts in \($0.files.joined(separator: ", "))." }
                ?? "Two lanes changed the same lines."
            return item(.crewResolve, "resolve", "The lanes conflict", detail, [
                VibeDevQuestionOption(label: "Resolve with an agent", style: .primary, action: .resolveWithAgent),
                VibeDevQuestionOption(label: "I'll resolve it", style: .plain, action: .resolveMyself),
            ])
        case .integrated where crew.pullRequestRequestId != nil && crew.pullRequestUrl == nil:
            var body = "All lanes are in \(crew.branch ?? "one branch")."
            if let spent = crew.spentUsd { body += " Spent \(VibeDevFormat.usd(spent))." }
            return item(.crewPullRequest, "pr", "Merged. Open a pull request?", body, [
                VibeDevQuestionOption(label: "Open pull request", style: .primary, action: .openCrewPullRequest),
                VibeDevQuestionOption(label: "Not now", style: .plain, action: .notNow),
            ])
        default:
            return nil
        }
    }

    static func runItem(_ run: VibeDevLiveRun) -> VibeDevNeedsYouItem {
        let eyebrow = (run.lane.map { "\($0) lane · " } ?? "") + run.projectName + " · needs you"
        return VibeDevNeedsYouItem(
            id: "run:\(run.runId)", kind: .run, eyebrow: eyebrow, title: run.displayTitle,
            body: run.step ?? run.summary ?? "Waiting for your review", projectID: run.projectId,
            crew: nil, run: run, options: [
                VibeDevQuestionOption(label: "Review", style: .primary, action: .reviewRun),
                VibeDevQuestionOption(label: "Approve", style: .plain, action: .approveRun),
            ],
            updatedAtMs: Int64((run.updatedDate?.timeIntervalSince1970 ?? 0) * 1000))
    }

    /// Crew questions first (newest first), then runs waiting on the owner in
    /// the server's order.
    static func needsYou(projects: [VibeDevProject], liveRuns: [VibeDevLiveRun]) -> [VibeDevNeedsYouItem] {
        let crewItems = projects.filter { !$0.archived }.flatMap { project in
            project.crews.compactMap { question(for: $0, projectID: project.projectId, projectName: project.name) }
        }
        .sorted { $0.updatedAtMs > $1.updatedAtMs }
        let runItems = liveRuns.filter { $0.state == .needsYou }.map(runItem)
        return crewItems + runItems
    }
}

/// Acts on options and run controls, with toasts and undo. Owned by a screen
/// model; `onChange` asks that screen to re-read.
@MainActor
final class VibeDevActions {
    let context: VibeDevContext
    var onChange: () -> Void

    init(context: VibeDevContext, onChange: @escaping () -> Void = {}) {
        self.context = context
        self.onChange = onChange
    }

    private var api: VibeDevServing { context.api }
    private var undo: VibeDevUndoCenter { context.undo }

    /// `hide(true)` while an undoable answer waits, `hide(false)` on undo or failure.
    func perform(_ option: VibeDevQuestionOption, of item: VibeDevNeedsYouItem,
                 hide: @escaping (Bool) -> Void, open: @escaping (VibeDevRoute) -> Void) {
        let project = item.projectID
        switch option.action {
        case .startCrew:
            crew(item, { try await self.api.crewAction(projectID: project, crewID: $0, action: .start) }, success: { "Started \($0.lanes.count) lanes" })
        case .dontStartCrew:
            undoable(item, "Crew not started", hide: hide) {
                _ = try await self.api.crewAction(projectID: project, crewID: $0, action: .stop)
            }
        case .keepLane(let index, let lane):
            crew(item, { try await self.api.chooseCrew(projectID: project, crewID: $0, laneIndex: index) }, success: { _ in "Keeping \(lane)" })
        case .keepNone:
            undoable(item, "Keeping none of the attempts", hide: hide) {
                _ = try await self.api.chooseCrew(projectID: project, crewID: $0, laneIndex: nil)
            }
        case .resolveWithAgent:
            crew(item, { try await self.api.crewAction(projectID: project, crewID: $0, action: .resolve) }, success: { _ in "Integrator started" })
        case .resolveMyself:
            answer(item, requestID: item.crew?.resolveRequestId, optionID: "manual",
                   done: "Left for you on the lane branches", hide: hide)
        case .openCrewPullRequest:
            crew(item, { try await self.api.crewAction(projectID: project, crewID: $0, action: .pullRequest) }, success: { _ in "Pull request opened" })
        case .notNow:
            answer(item, requestID: item.crew?.pullRequestRequestId, optionID: "later",
                   done: "Not now. The branch stays.", hide: hide)
        case .reviewRun:
            if let run = item.run { open(.run(run.runId, .changes)) }
        case .approveRun:
            guard let run = item.run else { return }
            hide(true)
            Task {
                let ok = await self.decide(approvalID: run.approvalId, taskIDs: [run.liveTaskId, run.runId], apply: true)
                if !ok { hide(false) }
            }
        }
    }

    /// Answer a sandboxed run's diff approval: directly by its id when the
    /// server named it, else through the Attention feed. False when nothing
    /// was sent.
    @discardableResult
    func decide(approvalID: String?, taskIDs: [String], apply: Bool) async -> Bool {
        if let approvalID, !approvalID.isEmpty {
            do {
                try await api.answerDiffApproval(approvalID: approvalID, apply: apply)
                undo.show(apply ? "Changes approved" : "Changes rejected")
                onChange()
                return true
            } catch {
                undo.show(Self.describe(error), isError: true)
                return false
            }
        }
        switch await context.approvals.decide(taskIDs: taskIDs, apply: apply) {
        case .sent:
            undo.show(apply ? "Changes approved" : "Changes rejected")
            onChange()
            return true
        case .notFound:
            undo.show("Open Attention to answer this run")
            AppActions.shared.requestAttention()
            return false
        case .failed(let message):
            undo.show(message, isError: true)
            return false
        }
    }

    /// Stop a live turn after the undo window. The undo center outlives the
    /// screen that owns these actions, so the commit holds the client and the
    /// center itself: leaving inside the window still sends the stop.
    func stop(liveTaskID: String, name: String, pending: @escaping (Bool) -> Void) {
        pending(true)
        let api = self.api, undo = self.undo, onChange = self.onChange
        undo.schedule("Stopping \(name)…", undo: { pending(false) }) {
            do {
                try await api.control(liveTaskID: liveTaskID, action: .stop, message: nil)
                undo.show("Stopped \(name)")
            } catch {
                pending(false)
                undo.show(Self.describe(error), isError: true)
            }
            onChange()
        }
    }

    /// Steer a live turn. Returns an error to show in the sheet, or nil.
    func steer(liveTaskID: String, name: String, message: String) async -> String? {
        let text = message.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return "Say what to change." }
        do {
            try await api.control(liveTaskID: liveTaskID, action: .steer, message: text)
            undo.show("Told \(name)")
            onChange()
            return nil
        } catch VibeDevAPIError.http(409, _) {
            return "This run is between turns. Open it to send a follow-up."
        } catch {
            return Self.describe(error)
        }
    }

    static func describe(_ error: Error) -> String {
        (error as? LocalizedError)?.errorDescription ?? error.localizedDescription
    }

    // MARK: Helpers

    private func crew(_ item: VibeDevNeedsYouItem, _ call: @escaping (String) async throws -> VibeDevCrew,
                      success: @escaping (VibeDevCrew) -> String) {
        guard let crewID = item.crew?.crewId else { return }
        Task {
            do {
                let crew = try await call(crewID)
                self.undo.show(success(crew))
            } catch {
                self.undo.show(Self.describe(error), isError: true)
            }
            self.onChange()
        }
    }

    private func undoable(_ item: VibeDevNeedsYouItem, _ message: String, hide: @escaping (Bool) -> Void,
                          _ call: @escaping (String) async throws -> Void) {
        guard let crewID = item.crew?.crewId else { return }
        hide(true)
        // Held strongly, like `stop`: the answer goes out even if the screen is gone.
        let undo = self.undo, onChange = self.onChange
        undo.schedule(message, undo: { hide(false) }) {
            do { try await call(crewID) } catch {
                hide(false)
                undo.show(Self.describe(error), isError: true)
            }
            onChange()
        }
    }

    private func answer(_ item: VibeDevNeedsYouItem, requestID: String?, optionID: String, done: String,
                        hide: @escaping (Bool) -> Void) {
        guard let requestID else { return }
        hide(true)
        Task {
            do {
                try await self.api.answerQuestion(requestID: requestID, optionID: optionID)
                self.undo.show(done)
            } catch {
                hide(false)
                self.undo.show(Self.describe(error), isError: true)
            }
            self.onChange()
        }
    }
}
