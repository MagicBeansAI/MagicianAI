import SwiftUI

/// New project: a name, a repository (suggested or typed), and trust.
struct VibeDevNewProjectSheet: View {
    let context: VibeDevContext
    let onCreated: (VibeDevProject) -> Void

    @ObservedObject private var theme = ThemeManager.shared
    @State private var name = ""
    @State private var repoPath = ""
    @State private var location: VibeDevTrust = .sandboxed
    @State private var baseBranch = ""
    @State private var suggestions: [VibeDevRepoSuggestion] = []
    @State private var sending = false
    @State private var error: String?

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                Text("New project")
                    .font(.themedDisplay(18, weight: .semibold))
                    .foregroundStyle(theme.textColor)
                    .padding(.top, 18)
                VibeDevField(label: "Name") {
                    field("Billing service", text: $name)
                }
                VibeDevField(label: "Repository") {
                    VStack(alignment: .leading, spacing: 8) {
                        if !suggestions.isEmpty {
                            VibeDevPills(options: suggestions.map { repo in
                                VibeDevPillOption(value: repo.path,
                                                  label: repo.project.map { "\(repo.name) · \($0)" } ?? repo.name)
                            }, selection: Binding(get: { repoPath }, set: { pick($0) }))
                        }
                        field("~/code/billing", text: $repoPath, mono: true)
                    }
                }
                VibeDevField(label: "Work location") {
                    VibeDevPills(options: [
                        VibeDevPillOption(value: VibeDevTrust.original, label: "Original folder"),
                        VibeDevPillOption(value: VibeDevTrust.sandboxed, label: "Isolated workspace"),
                        VibeDevPillOption(value: VibeDevTrust.trusted, label: "Isolated branch")
                    ], selection: $location)
                    Text("Original folder keeps your current branch and uncommitted files. Review exact diffs before they apply. Independent of Super god mode.")
                        .font(.themed(12)).foregroundStyle(theme.secondaryTextColor)
                }
                if location == .trusted {
                    VibeDevField(label: "Base branch (optional)") {
                        field("the repo's current branch", text: $baseBranch, mono: true)
                    }
                }
                if let error {
                    Text(error).font(.themed(13)).foregroundStyle(theme.dangerColor)
                }
                Button(action: submit) {
                    if sending { ProgressView().tint(theme.onAccentColor) } else { Text("Create project") }
                }
                .buttonStyle(VibeDevButtonStyle(style: .primary, expand: true, large: true))
                .disabled(sending || repoPath.trimmingCharacters(in: .whitespaces).isEmpty)
            }
            .padding(.horizontal, 16)
            .padding(.bottom, 24)
        }
        .scrollDismissesKeyboard(.interactively)
        .background(theme.cardColor.ignoresSafeArea())
        .task { suggestions = (try? await context.api.repoSuggestions()) ?? [] }
    }

    private func field(_ placeholder: String, text: Binding<String>, mono: Bool = false) -> some View {
        TextField(placeholder, text: text)
            .font(mono ? .themedMono(14) : .themed(15))
            .foregroundStyle(theme.textColor)
            .textInputAutocapitalization(mono ? .never : .sentences)
            .autocorrectionDisabled(mono)
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
            .background(RoundedRectangle(cornerRadius: 12).fill(theme.surfaceColor))
            .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(theme.controlBorderColor, lineWidth: 1))
    }

    private func pick(_ path: String) {
        repoPath = path
        if name.trimmingCharacters(in: .whitespaces).isEmpty,
           let suggestion = suggestions.first(where: { $0.path == path }) {
            name = suggestion.name
        }
    }

    private func submit() {
        let path = repoPath.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !path.isEmpty else { return }
        let typed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let projectName = typed.isEmpty ? (path.split(separator: "/").last.map(String.init) ?? path) : typed
        let branch = baseBranch.trimmingCharacters(in: .whitespacesAndNewlines)
        sending = true
        error = nil
        let api = context.api
        Task { @MainActor in
            defer { sending = false }
            do {
                var project = try await api.createProject(name: projectName, repoPath: path)
                if location != .sandboxed || !branch.isEmpty {
                    do {
                        project = try await api.updateProject(id: project.projectId,
                                                              trust: location,
                                                              baseBranch: branch.isEmpty ? nil : branch)
                    } catch {
                        context.undo.show("Created \(projectName), sandboxed: \(VibeDevActions.describe(error))", isError: true)
                        context.remember(project: project)
                        onCreated(project)
                        return
                    }
                }
                context.remember(project: project)
                context.undo.show("Created \(projectName)")
                onCreated(project)
            } catch {
                self.error = VibeDevActions.describe(error)
            }
        }
    }
}
