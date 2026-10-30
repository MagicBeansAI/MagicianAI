import SwiftUI

/// A VibeDev crew (not a Work crew): goal, status, spend of budget, one row
/// per lane, and the crew's current question as its buttons. Tapping a lane
/// opens its run.
struct VibeDevCrewCard: View {
    let crew: VibeDevCrew
    /// The open question, when the crew is waiting on the owner.
    var question: VibeDevNeedsYouItem?
    /// The project name on the eyebrow (Now shows crews of every project).
    var projectName: String?
    var onOption: (VibeDevQuestionOption) -> Void = { _ in }
    var onOpenRun: (String) -> Void = { _ in }

    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                VibeDevStateDot(crew: crew.status)
                VibeDevTag(text: crew.mode == .race ? "VibeDev race" : "VibeDev crew", icon: "person.3.sequence.fill")
                Text(crew.status.label)
                    .font(.themed(11, weight: .bold))
                    .foregroundStyle(theme.secondaryTextColor)
                if let projectName, !projectName.isEmpty {
                    Text("· \(projectName)")
                        .font(.themed(11, weight: .semibold))
                        .foregroundStyle(theme.secondaryTextColor)
                        .vibeDevLineLimit(1)
                }
                if !crew.isFinished {
                    Spacer(minLength: 4)
                    // Follow: the crew as its own Live Activity until it ends.
                    WorkFollowButton(seed: FollowedItemActivity.crewSeed(
                        crewID: crew.crewId, projectID: nil, goal: crew.goal,
                        projectName: projectName, needsYou: question != nil))
                        .vibeDevControlOutline()
                }
            }
            Text(crew.goal)
                .font(.themedDisplay(15, weight: .semibold))
                .foregroundStyle(theme.textColor)
                .vibeDevLineLimit(2)
            VibeDevCrewLanes(crew: crew, onOpenRun: onOpenRun)
            if let conflict = crew.conflict, crew.status == .conflict {
                Text("\(conflict.lane) conflicts in \(conflict.files.joined(separator: ", ")).")
                    .font(.themed(12))
                    .foregroundStyle(theme.warningColor)
            }
            if let branch = crew.branch {
                HStack(spacing: 4) {
                    Text("Branch").font(.themed(12)).foregroundStyle(theme.secondaryTextColor)
                    Text(branch).font(.themedMono(12)).foregroundStyle(theme.textColor).vibeDevLineLimit(1)
                }
            }
            if let url = crew.pullRequestUrl.flatMap(URL.init(string:)) {
                Link(destination: url) {
                    Label("Pull request", systemImage: "arrow.triangle.pull")
                        .font(.themed(13, weight: .bold))
                        .foregroundStyle(theme.accentColor)
                }
            }
            if let note = crew.note, question == nil, crew.status != .running {
                Text(note).font(.themed(12)).foregroundStyle(theme.secondaryTextColor)
            }
            if let question {
                VibeDevOptionButtons(options: question.options, onOption: onOption)
            }
        }
        .vibeDevCard(highlight: question == nil ? nil : theme.warningColor)
        .accessibilityElement(children: .contain)
    }
}

/// The lane rows and the budget bar, shared by the crew card and the
/// Needs-you card of a crew question.
struct VibeDevCrewLanes: View {
    let crew: VibeDevCrew
    var onOpenRun: (String) -> Void = { _ in }
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            ForEach(Array(crew.lanes.enumerated()), id: \.offset) { index, lane in
                Button {
                    if let run = lane.runTaskId { onOpenRun(run) }
                } label: {
                    HStack(spacing: 8) {
                        VibeDevStateDot(lane: lane.status)
                        HStack(spacing: 4) {
                            if let role = lane.role.label {
                                Text(role.uppercased())
                                    .font(.themed(10, weight: .bold))
                                    .foregroundStyle(theme.accentColor)
                            }
                            Text(lane.lane)
                                .font(.themed(13))
                                .foregroundStyle(theme.textColor)
                                .vibeDevLineLimit(1)
                            if crew.winner == index {
                                Image(systemName: "checkmark.seal.fill")
                                    .font(.system(size: 11))
                                    .foregroundStyle(theme.successColor)
                                    .accessibilityLabel("Kept")
                            }
                        }
                        Spacer(minLength: 4)
                        Text(lane.status.label)
                            .font(.themed(11))
                            .foregroundStyle(theme.secondaryTextColor)
                        if lane.runTaskId != nil {
                            Image(systemName: "chevron.right")
                                .font(.system(size: 10, weight: .semibold))
                                .foregroundStyle(theme.secondaryTextColor)
                        }
                    }
                    .frame(minHeight: Lively.tapMin)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .disabled(lane.runTaskId == nil)
                .accessibilityHint(lane.runTaskId == nil ? "No run yet" : "Opens the lane's run")
                .accessibilityLabel("\(lane.role.label.map { "\($0), " } ?? "")\(lane.lane), \(lane.status.label)")
            }
            if let budget = crew.budgetUsd, budget > 0 {
                let spent = crew.spentUsd ?? 0
                Text("\(VibeDevFormat.usd(spent)) of \(VibeDevFormat.usd(budget))")
                    .font(.themed(12))
                    .foregroundStyle(theme.secondaryTextColor)
                GeometryReader { proxy in
                    ZStack(alignment: .leading) {
                        Capsule().fill(theme.surfaceColor)
                        Capsule().fill(theme.accentColor)
                            .frame(width: proxy.size.width * min(1, max(0, spent / budget)))
                    }
                }
                .frame(height: 4)
                .accessibilityHidden(true)
            } else if let spent = crew.spentUsd, spent > 0 {
                Text("Spent \(VibeDevFormat.usd(spent))")
                    .font(.themed(12))
                    .foregroundStyle(theme.secondaryTextColor)
            }
        }
    }
}

/// A question's options as wrapped pill buttons.
struct VibeDevOptionButtons: View {
    let options: [VibeDevQuestionOption]
    let onOption: (VibeDevQuestionOption) -> Void

    var body: some View {
        VibeDevFlow(spacing: 6) {
            ForEach(options) { option in
                Button(option.label) { onOption(option) }
                    .buttonStyle(VibeDevButtonStyle(style: option.style))
            }
        }
    }
}

/// A Needs-you card: what is asked in one line, its options as buttons.
/// Tapping the body opens the run or the project.
struct VibeDevAskCard: View {
    let item: VibeDevNeedsYouItem
    var onOption: (VibeDevQuestionOption) -> Void
    var onOpen: () -> Void
    var onOpenRun: (String) -> Void = { _ in }
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Button(action: onOpen) {
                VStack(alignment: .leading, spacing: 6) {
                    HStack(spacing: 6) {
                        VibeDevStateDot(run: .needsYou)
                        if item.crew != nil {
                            Image(systemName: "person.3.sequence.fill").font(.system(size: 10, weight: .bold))
                        }
                        Text(item.eyebrow)
                            .font(.themed(11, weight: .bold))
                            .vibeDevLineLimit(1)
                    }
                    .foregroundStyle(theme.warningColor)
                    Text(item.title)
                        .font(.themedDisplay(15, weight: .semibold))
                        .foregroundStyle(theme.textColor)
                        .vibeDevLineLimit(2)
                        .multilineTextAlignment(.leading)
                    Text(item.body)
                        .font(.themed(12))
                        .foregroundStyle(theme.secondaryTextColor)
                        .vibeDevLineLimit(2)
                        .multilineTextAlignment(.leading)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            if let crew = item.crew {
                VibeDevCrewLanes(crew: crew, onOpenRun: onOpenRun)
            }
            VibeDevOptionButtons(options: item.options, onOption: onOption)
        }
        .vibeDevCard(highlight: theme.warningColor)
    }
}
