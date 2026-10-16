import SwiftUI

/// New run, Follow up, and (on a trusted project) a new VibeDev crew.
struct VibeDevNewRunSheet: View {
    enum Outcome { case run(String), crew }

    let context: VibeDevContext
    let seed: VibeDevNewRunSeed
    let onDone: (Outcome) -> Void

    @StateObject private var dictation = DictationController()
    @ObservedObject private var theme = ThemeManager.shared
    @State private var text = ""
    @State private var projectID: String
    @State private var mode: VibeDevTurnMode = .build
    /// "" is the catalog default.
    @State private var profileID = ""
    @State private var budget = ""
    @State private var crew: Bool
    @State private var crewMode: VibeDevCrewMode = .split
    @State private var attempts = 3
    @State private var review = true
    @State private var projects: [VibeDevProject]
    @State private var profiles: [VibeDevCodingProfile] = []
    @State private var sending = false
    @State private var error: String?
    /// One id per submission, so a retried send returns the same run.
    @State private var clientRunID = UUID().uuidString.lowercased()

    init(context: VibeDevContext, seed: VibeDevNewRunSeed, onDone: @escaping (Outcome) -> Void) {
        self.context = context
        self.seed = seed
        self.onDone = onDone
        let list = context.projectList.filter { !$0.archived }
        _projects = State(initialValue: list)
        _projectID = State(initialValue: seed.projectID ?? list.first?.projectId ?? "")
        _crew = State(initialValue: seed.crew)
        _profiles = State(initialValue: context.profiles ?? [])
    }

    private var isFollowUp: Bool { seed.followUp != nil }
    private var project: VibeDevProject? { projects.first { $0.projectId == projectID } ?? context.knownProjects[projectID] }
    private var canCrew: Bool { !isFollowUp && (project?.isTrusted ?? false) }
    private var makesCrew: Bool { canCrew && crew }
    private var budgetValue: Double? {
        Double(budget.replacingOccurrences(of: "$", with: "").replacingOccurrences(of: ",", with: ".")
            .trimmingCharacters(in: .whitespaces))
    }

    private var title: String {
        if isFollowUp { return "Follow up" }
        return makesCrew ? "New VibeDev crew" : "New run"
    }

    private var submitLabel: String {
        if makesCrew { return crewMode == .race ? "Set up race" : "Plan lanes" }
        return isFollowUp ? "Send follow-up" : "Start run"
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                Text(title)
                    .font(.themedDisplay(18, weight: .semibold))
                    .foregroundStyle(theme.textColor)
                    .padding(.top, 18)
                VibeDevPromptField(
                    placeholder: makesCrew ? "What should the crew build?" : (isFollowUp ? "What next?" : "What should it build?"),
                    text: $text, dictation: dictation)
                if !isFollowUp && projects.count > 1 {
                    VibeDevField(label: "Project") {
                        VibeDevPills(options: projects.map { VibeDevPillOption(value: $0.projectId, label: $0.name) },
                                     selection: $projectID)
                    }
                }
                if !isFollowUp && !makesCrew {
                    VibeDevField(label: "Mode") {
                        VibeDevPills(options: [VibeDevPillOption(value: VibeDevTurnMode.build, label: "Build"),
                                               VibeDevPillOption(value: VibeDevTurnMode.discuss, label: "Discuss")],
                                     selection: $mode)
                    }
                }
                if !makesCrew {
                    VibeDevField(label: "Engine") {
                        VibeDevPills(options: [VibeDevPillOption(value: "", label: "Default")] +
                                     profiles.filter(\.selectable).map { VibeDevPillOption(value: $0.id, label: $0.label) },
                                     selection: $profileID)
                    }
                }
                if canCrew {
                    VibeDevToggleRow(title: "VibeDev crew",
                                     detail: "Split the goal into lanes that run at once, or race engines at it.",
                                     isOn: $crew)
                }
                if makesCrew {
                    VibeDevField(label: "Crew") {
                        VibeDevPills(options: [VibeDevPillOption(value: VibeDevCrewMode.split, label: "Split the goal"),
                                               VibeDevPillOption(value: VibeDevCrewMode.race, label: "Race engines")],
                                     selection: $crewMode)
                    }
                    if crewMode == .race {
                        Stepper(value: $attempts, in: 2...8) {
                            Text("\(attempts) attempts")
                                .font(.themed(14, weight: .semibold))
                                .foregroundStyle(theme.textColor)
                        }
                    }
                    VibeDevToggleRow(title: "Review first", detail: "Review the merged branch before the pull request.",
                                     isOn: $review)
                }
                VibeDevField(label: makesCrew ? "Crew budget (optional)" : "Budget (optional)") {
                    TextField("$ no limit", text: $budget)
                        .keyboardType(.decimalPad)
                        .font(.themed(15))
                        .foregroundStyle(theme.textColor)
                        .padding(.horizontal, 12)
                        .padding(.vertical, 10)
                        .background(RoundedRectangle(cornerRadius: 12).fill(theme.surfaceColor))
                        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(theme.controlBorderColor, lineWidth: 1))
                }
                if let error {
                    Text(error)
                        .font(.themed(13))
                        .foregroundStyle(theme.dangerColor)
                }
                Button(action: submit) {
                    if sending { ProgressView().tint(theme.onAccentColor) } else { Text(submitLabel) }
                }
                .buttonStyle(VibeDevButtonStyle(style: .primary, expand: true, large: true))
                .disabled(sending || text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || projectID.isEmpty)
            }
            .padding(.horizontal, 16)
            .padding(.bottom, 24)
        }
        .scrollDismissesKeyboard(.interactively)
        .background(theme.cardColor.ignoresSafeArea())
        .task { await loadPickers() }
        .onDisappear { if dictation.isRecording { dictation.finishLive { _ in } } }
    }

    private func loadPickers() async {
        if projects.isEmpty, let list = try? await context.api.projects() {
            context.remember(list.projects)
            projects = list.projects.filter { !$0.archived }
            if projectID.isEmpty { projectID = projects.first?.projectId ?? "" }
        }
        if context.profiles == nil {
            // The picker falls back to "Default" alone when the catalog is unavailable.
            let loaded = (try? await context.api.codingProfiles()) ?? []
            context.profiles = loaded
            profiles = loaded
        }
    }

    private func submit() {
        let prompt = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !prompt.isEmpty, !projectID.isEmpty else { return }
        sending = true
        error = nil
        let api = context.api
        let undo = context.undo
        Task { @MainActor in
            defer { sending = false }
            do {
                if makesCrew {
                    let request = VibeDevCreateCrewRequest(goal: prompt, mode: crewMode,
                                                           racers: crewMode == .race ? attempts : nil,
                                                           review: review, budgetUsd: budgetValue)
                    _ = try await api.createCrew(projectID: projectID, request: request)
                    undo.show(crewMode == .race ? "Race set up; start it from Now" : "Lanes planned; start the crew from Now")
                    onDone(.crew)
                } else {
                    var request = VibeDevStartRunRequest(prompt: prompt, projectId: projectID, mode: isFollowUp ? .build : mode)
                    request.clientRunId = clientRunID
                    if let followUp = seed.followUp {
                        request.parentTaskId = followUp.parentTaskID
                        request.threaded = true
                    }
                    request.codingProfileId = profileID.isEmpty ? nil : profileID
                    request.costBudgetUsd = budgetValue
                    let response = try await api.startRun(request)
                    undo.show(response.replayed ? "Already started" : (isFollowUp ? "Follow-up sent" : "Run started"))
                    onDone(.run(response.taskId))
                }
            } catch {
                self.error = VibeDevActions.describe(error)
            }
        }
    }
}
