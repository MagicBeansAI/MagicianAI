import SwiftUI

/// One project: header, VibeDev crews, and its runs (paged, searched on the
/// server). New run at the bottom; New crew too on a trusted project.
struct VibeDevProjectView: View {
    @StateObject private var model: VibeDevProjectModel
    @EnvironmentObject private var navigator: VibeDevNavigator
    @ObservedObject private var theme = ThemeManager.shared
    @State private var searchText = ""
    @State private var showFinished = false

    init(projectID: String, context: VibeDevContext) {
        _model = StateObject(wrappedValue: VibeDevProjectModel(projectID: projectID, context: context))
    }

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 10) {
                header
                VibeDevStaleNote(lastUpdated: model.lastUpdated, failed: model.refreshFailed)
                if let error = model.errorMessage, model.rows.isEmpty {
                    VibeDevEmpty(text: error)
                }
                crews
                runs
            }
            .padding(.horizontal, 16)
            .padding(.top, 4)
            .padding(.bottom, 24)
        }
        .background(theme.backgroundColor.ignoresSafeArea())
        .refreshable { await model.refresh() }
        .navigationTitle(model.project?.name ?? "Project")
        .navigationBarTitleDisplayMode(.inline)
        .toolbarBackground(theme.backgroundColor, for: .navigationBar)
        .safeAreaInset(edge: .bottom) {
            VibeDevBottomBar {
                if model.isTrusted {
                    Button("New crew") {
                        navigator.present(.newRun(VibeDevNewRunSeed(projectID: model.projectID, crew: true)))
                    }
                    .buttonStyle(VibeDevButtonStyle(style: .plain, expand: true, large: true))
                }
                Button("New run") {
                    navigator.present(.newRun(VibeDevNewRunSeed(projectID: model.projectID)))
                }
                .buttonStyle(VibeDevButtonStyle(style: .primary, expand: true, large: true))
            }
        }
        .task { await model.refresh() }
        .task(id: searchText) {
            // Debounce typing; the server does the search.
            try? await Task.sleep(nanoseconds: 400_000_000)
            guard !Task.isCancelled else { return }
            await model.search(searchText)
        }
        .onAppear { model.startLive() }
        .onDisappear { model.stopLive() }
        .onChange(of: navigator.revision) { _, _ in Task { await model.refresh() } }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text(model.project?.name ?? "")
                    .font(.themedDisplay(20, weight: .semibold))
                    .foregroundStyle(theme.textColor)
                Spacer()
                if let project = model.project { VibeDevTrustBadge(trusted: project.isTrusted) }
            }
            if let project = model.project {
                VibeDevKeyValues(rows: [
                    ("Repo", project.repoLabel.isEmpty ? "—" : project.repoLabel),
                    ("Base", project.baseBranch ?? (project.isTrusted ? "current branch" : "—")),
                ])
            }
        }
        .vibeDevCard()
    }

    @ViewBuilder
    private var crews: some View {
        let open = model.openCrews
        let finished = model.finishedCrews
        if !open.isEmpty || !finished.isEmpty {
            VibeDevSectionLabel(title: "VibeDev crews", count: open.count)
            ForEach(open) { crew in
                crewCard(crew)
            }
            if !finished.isEmpty {
                Button {
                    withAnimation(.easeInOut(duration: 0.2)) { showFinished.toggle() }
                } label: {
                    HStack(spacing: 4) {
                        Image(systemName: showFinished ? "chevron.down" : "chevron.right")
                            .font(.system(size: 10, weight: .semibold))
                        Text("\(finished.count) finished")
                            .font(.themed(12, weight: .semibold))
                    }
                    .foregroundStyle(theme.secondaryTextColor)
                    .frame(minHeight: Lively.tapMin)
                }
                .buttonStyle(.plain)
                if showFinished {
                    ForEach(finished) { crew in crewCard(crew) }
                }
            }
        }
    }

    private func crewCard(_ crew: VibeDevCrew) -> some View {
        let question = model.question(for: crew)
        return VibeDevCrewCard(
            crew: crew, question: question,
            onOption: { option in
                if let question { model.perform(option, of: question, open: { navigator.open($0) }) }
            },
            onOpenRun: { navigator.open(.run($0, .turns)) })
    }

    @ViewBuilder
    private var runs: some View {
        VibeDevSectionLabel(title: "Runs", count: model.totalRuns)
        HStack(spacing: 8) {
            Image(systemName: "magnifyingglass")
                .foregroundStyle(theme.secondaryTextColor)
            TextField("Search runs", text: $searchText)
                .font(.themed(14))
                .foregroundStyle(theme.textColor)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .submitLabel(.search)
            if !searchText.isEmpty {
                Button { searchText = "" } label: {
                    Image(systemName: "xmark.circle.fill").foregroundStyle(theme.secondaryTextColor)
                        .vibeDevHitTarget()
                }
                .accessibilityLabel("Clear search")
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 9)
        .background(RoundedRectangle(cornerRadius: 12).fill(theme.surfaceColor))
        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(theme.controlBorderColor, lineWidth: 1))

        if model.rows.isEmpty, model.loaded, model.errorMessage == nil {
            VibeDevEmpty(text: model.query.isEmpty ? "No runs yet. Start one below." : "No run matches “\(model.query)”.")
        }
        ForEach(model.rows) { row in
            Button { navigator.open(.run(row.id, .turns)) } label: {
                VibeDevRunRowView(row: row)
            }
            .buttonStyle(.plain)
            .onAppear {
                if row.id == model.rows.last?.id { Task { await model.loadMore() } }
            }
        }
        if model.hasMore {
            Button {
                Task { await model.loadMore() }
            } label: {
                if model.loadingMore { ProgressView().tint(theme.accentColor) } else { Text("Load more") }
            }
            .buttonStyle(VibeDevButtonStyle(style: .plain, expand: true))
            .disabled(model.loadingMore)
        }
    }
}

/// A run on a project's list: state dot, title, turns, updated, lane.
struct VibeDevRunRowView: View {
    let row: VibeDevRunRow
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        HStack(spacing: 10) {
            VibeDevStateDot(run: row.state)
            VStack(alignment: .leading, spacing: 2) {
                Text(row.title)
                    .font(.themedDisplay(14, weight: .semibold))
                    .foregroundStyle(theme.textColor)
                    .vibeDevLineLimit(1)
                HStack(spacing: 6) {
                    if let lane = row.link?.lane { VibeDevTag(text: "\(lane) lane") }
                    Text(row.turnCount == 1 ? "1 turn" : "\(row.turnCount) turns")
                    Text("·")
                    Text(row.state.label)
                }
                .font(.themed(12))
                .foregroundStyle(theme.secondaryTextColor)
                .vibeDevLineLimit(1)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            Text(VibeDevTime.age(since: row.updatedDate))
                .font(.themedMono(11, weight: .medium))
                .foregroundStyle(theme.secondaryTextColor)
            Image(systemName: "chevron.right")
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(theme.secondaryTextColor)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 11)
        .background(theme.cardColor)
        .clipShape(RoundedRectangle(cornerRadius: Lively.cardRadius, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Lively.cardRadius, style: .continuous)
            .strokeBorder(theme.cardBorderColor, lineWidth: 1))
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
    }
}
