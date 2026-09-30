import SwiftUI
import UIKit
import MarkdownUI

extension ActivityInspectTarget {
    /// A target derived from the Steps strip is execution-scoped by
    /// definition; task cards use the default task-details presentation.
    var detailPresentation: TaskDetailPresentation { .runInspection }
}

/// Native counterpart of web RequestActivityCard. The compact disclosure keeps
/// the newest five coalesced rows in the bubble; Show all opens the complete
/// chronological log, and run/file/link actions preserve their web semantics.
struct ActivityTimelineView: View {
    let rows: [ActivityRow]
    let isLive: Bool
    let sessionId: String?
    @ObservedObject var theme: ThemeManager

    @State private var expanded = false
    @State private var userToggledExpand = false
    @State private var showAll = false
    @State private var showRun = false
    @State private var openedArtifact: ArtifactRef?
    @State private var resultTarget: ActivityResultTarget?
    @State private var actionError: String?

    init(
        rows: [ActivityRow],
        isLive: Bool = false,
        sessionId: String? = nil,
        theme: ThemeManager
    ) {
        self.rows = rows
        self.isLive = isLive
        self.sessionId = sessionId
        self.theme = theme
    }

    private var previewRows: [ActivityRow] { activityPreviewRows(rows) }
    private var hiddenCount: Int { max(0, rows.count - previewRows.count) }
    // Header liveness is authoritative turn state, not a guess from leaf rows.
    // A durable replay can legitimately miss a leaf terminal event; treating
    // that stale row as turn liveness would resurrect the endless spinner this
    // surface previously fixed.
    private var isRunning: Bool { isLive }
    private var inspectTarget: ActivityInspectTarget? { activityInspectTarget(rows) }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 6) {
                Button(action: toggleExpanded) {
                    HStack(spacing: 6) {
                        Image(systemName: expanded ? "chevron.down" : "chevron.right")
                            .font(.system(size: 10, weight: .bold))
                        Text("Steps")
                            .font(.themed(12, weight: .semibold))
                        Text("\(rows.count)")
                            .font(.themed(11, weight: .medium))
                            .foregroundColor(theme.secondaryTextColor)
                        if isRunning {
                            ProgressView().controlSize(.mini)
                        }
                    }
                    // Activity is supporting context beneath the answer. Keep
                    // the disclosure legible without competing with the reply.
                    .foregroundColor(theme.secondaryTextColor)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(expanded ? "Hide steps" : "Show steps")

                Spacer(minLength: 2)

                if expanded, hiddenCount > 0 {
                    headerAction("All", systemImage: "list.bullet") { showAll = true }
                        .accessibilityLabel("Show all \(rows.count) steps")
                }
                if inspectTarget != nil {
                    headerAction("Run", systemImage: "arrow.up.right.square") { showRun = true }
                        .accessibilityLabel("Inspect run")
                }
                if isRunning, let executionId = inspectTarget?.executionId {
                    ActivityStopRunButton(executionId: executionId, theme: theme)
                        .id(executionId)
                }
            }
            .padding(.vertical, 7)

            if expanded {
                VStack(alignment: .leading, spacing: 8) {
                    if rows.isEmpty, isLive {
                        HStack(spacing: 8) {
                            ProgressView().controlSize(.mini)
                            Text("Waiting for the first activity update…")
                                .font(.themed(11))
                                .foregroundColor(theme.secondaryTextColor)
                        }
                        .padding(.vertical, 2)
                    }
                    ForEach(previewRows) { row in
                        ActivityTimelineRow(
                            row: row,
                            theme: theme,
                            onPathAction: performPathAction,
                            onOpenResult: openResult
                        )
                    }
                }
                .padding(.leading, 2)
                .padding(.bottom, 8)
                .transition(.opacity.combined(with: .move(edge: .top)))
            }
        }
        .padding(.horizontal, 10)
        .background(theme.controlColor)
        .clipShape(RoundedRectangle(cornerRadius: 8))
        .overlay {
            RoundedRectangle(cornerRadius: 8)
                .stroke(theme.controlBorderColor, lineWidth: 1)
        }
        .environment(\.openURL, OpenURLAction { openActivityURL($0) })
        .onAppear {
            if !userToggledExpand, isRunning { expanded = true }
        }
        .onChange(of: isRunning) { _, running in
            if !userToggledExpand { expanded = running }
        }
        .sheet(isPresented: $showAll) {
            ActivityLogSheet(
                rows: rows,
                sessionId: sessionId,
                theme: theme,
                onPathAction: performPathAction,
                onOpenURL: { url in
                    showAll = false
                    DispatchQueue.main.async { _ = openActivityURL(url) }
                }
            )
        }
        .sheet(isPresented: $showRun) { runSheet }
        .sheet(item: $openedArtifact) { ArtifactViewer(artifact: $0) }
        .sheet(item: $resultTarget) { target in
            ActivityResultViewer(target: target, theme: theme)
        }
        .alert("Action failed", isPresented: Binding(
            get: { actionError != nil },
            set: { if !$0 { actionError = nil } }
        )) {
            Button("OK", role: .cancel) { actionError = nil }
        } message: {
            Text(actionError ?? "The action could not be completed.")
        }
    }

    private func toggleExpanded() {
        userToggledExpand = true
        withAnimation(.easeInOut(duration: 0.15)) { expanded.toggle() }
    }

    private func headerAction(
        _ title: String,
        systemImage: String,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            Label(title, systemImage: systemImage)
                .labelStyle(.titleAndIcon)
                .font(.themed(10, weight: .semibold))
                .foregroundColor(theme.accentColor)
                .padding(.horizontal, 7)
                .frame(height: 26)
                .background(theme.surfaceColor)
                .clipShape(Capsule())
                .overlay(Capsule().stroke(theme.controlBorderColor, lineWidth: 1))
        }
        .buttonStyle(.plain)
    }

    @ViewBuilder
    private var runSheet: some View {
        if let target = inspectTarget {
            DeepWorkPanel(task: TaskStatusModel(
                taskId: target.taskId,
                title: "Task run",
                status: isRunning ? "running" : "completed",
                steps: rows.map(\.label),
                executionId: target.executionId,
                activeRootExecutionId: isRunning ? target.executionId : nil
            ), presentation: target.detailPresentation)
        }
    }

    private func openActivityURL(_ url: URL) -> OpenURLAction.Result {
        switch classifyActivityLink(url.absoluteString) {
        case .external(let raw):
            guard let target = URL(string: raw) else { return .discarded }
            UIApplication.shared.open(target)
        case .task(let id):
            AppActions.shared.requestTask(id)
        case .execution(let executionId):
            if inspectTarget?.executionId == executionId {
                showRun = true
            } else {
                let encoded = executionId.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed)
                    ?? executionId
                openedArtifact = .direct(
                    url: "/debug?execution_id=\(encoded)", mime: "text/html", name: "Run"
                )
            }
        case .thread(let id):
            AppActions.shared.requestThread(id)
        case .attention(let id):
            AppActions.shared.requestAttention(itemID: id)
        case .today(let rawSection):
            AppActions.shared.requestToday(section: rawSection.flatMap(TodaySection.init(rawValue:)))
        case .settings:
            AppActions.shared.requestSettings()
        case .observe:
            AppActions.shared.requestObserve()
        case .taskOutput(let taskId, let relativePath):
            openedArtifact = .taskOutput(taskId: taskId, relativePath: relativePath, mime: nil)
        case .artifact(let raw):
            openedArtifact = .direct(url: raw, mime: nil, name: URL(string: raw)?.lastPathComponent)
        case .file(let path):
            performPathAction(path, .file)
        case .webRoute(let route):
            openedArtifact = .direct(url: route, mime: "text/html", name: MagicianAccess.productName)
        case .unknown:
            return .discarded
        }
        return .handled
    }

    private func performPathAction(_ path: String, _ action: ActivityPathAction) {
        guard let sessionId, !sessionId.isEmpty else {
            actionError = "This file is not attached to a chat session."
            return
        }
        Task {
            do {
                try await ActivityPathClient.perform(
                    sessionId: sessionId,
                    absolutePath: path,
                    action: action
                )
            } catch {
                await MainActor.run {
                    actionError = action == .file
                        ? "The file could not be opened."
                        : "The folder could not be revealed."
                }
            }
        }
    }

    private func openResult(_ row: ActivityRow) {
        guard let resultRef = row.resultRef else {
            actionError = "This complete result is no longer attached to a chat session."
            return
        }
        guard row.resultOwner != nil || row.taskId != nil || sessionId != nil else {
            actionError = "This complete result is no longer attached to a chat or task owner."
            return
        }
        resultTarget = ActivityResultTarget(
            sessionId: sessionId,
            resultRef: resultRef,
            title: row.label,
            contentHash: row.resultHash,
            sizeBytes: row.resultSizeBytes,
            owner: row.resultOwner,
            taskId: row.taskId,
            executionId: row.executionId
        )
    }
}

private struct ActivityTimelineRow: View {
    let row: ActivityRow
    @ObservedObject var theme: ThemeManager
    let onPathAction: (String, ActivityPathAction) -> Void
    let onOpenResult: (ActivityRow) -> Void

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: iconName)
                .font(.system(size: 11, weight: .semibold))
                .foregroundColor(statusColor)
                .frame(width: 16)
                .padding(.top, 2)

            VStack(alignment: .leading, spacing: 4) {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(row.label)
                        .font(.themed(12, weight: .medium))
                        .foregroundColor(theme.secondaryTextColor)
                        .fixedSize(horizontal: false, vertical: true)
                    if let ms = row.durationMs {
                        Text(formatLatency(ms))
                            .font(.themed(10))
                            .foregroundColor(theme.secondaryTextColor)
                    }
                }

                ForEach(row.files) { file in
                    HStack(spacing: 3) {
                        Button { onPathAction(file.absolutePath, .file) } label: {
                            Label(file.label, systemImage: "doc")
                                .font(.themed(11, weight: .semibold))
                                .lineLimit(1)
                                .foregroundColor(theme.accentColor)
                        }
                        .buttonStyle(.plain)

                        Menu {
                            Button { onPathAction(file.absolutePath, .file) } label: {
                                Label("Open file", systemImage: "arrow.up.forward.app")
                            }
                            Button { onPathAction(file.absolutePath, .folder) } label: {
                                Label("Reveal folder", systemImage: "folder")
                            }
                        } label: {
                            Image(systemName: "ellipsis.circle")
                                .font(.system(size: 13))
                                .foregroundColor(theme.secondaryTextColor)
                                .frame(width: 26, height: 24)
                        }
                        .accessibilityLabel("File actions for \(file.label)")
                    }
                    .padding(.horizontal, 7)
                    .padding(.vertical, 4)
                    .background(theme.accentColor.opacity(0.09))
                    .clipShape(RoundedRectangle(cornerRadius: 6))
                    .overlay {
                        RoundedRectangle(cornerRadius: 6)
                            .stroke(theme.accentColor.opacity(0.22), lineWidth: 1)
                    }
                }

                if row.resultRef != nil {
                    Button { onOpenResult(row) } label: {
                        Label("Open complete result", systemImage: "doc.text.magnifyingglass")
                            .font(.themed(11, weight: .semibold))
                            .foregroundColor(theme.accentColor)
                            .padding(.horizontal, 8)
                            .frame(height: 28)
                            .background(theme.accentColor.opacity(0.09))
                            .clipShape(RoundedRectangle(cornerRadius: 6))
                            .overlay {
                                RoundedRectangle(cornerRadius: 6)
                                    .stroke(theme.accentColor.opacity(0.22), lineWidth: 1)
                            }
                    }
                    .buttonStyle(.plain)
                }

                if let detail = row.detail, !detail.isEmpty {
                    Markdown(detail)
                        .markdownTextStyle {
                            FontFamily(.custom(theme.fontName))
                            ForegroundColor(theme.secondaryTextColor)
                        }
                        .font(.themed(11))
                        .tint(theme.accentColor)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer(minLength: 0)
        }
    }

    private var iconName: String {
        if row.status == .failed { return "exclamationmark.triangle.fill" }
        if row.status == .waiting { return "pause.circle.fill" }
        if row.status == .running { return "circle.dotted.circle" }
        switch row.kind {
        case .llm: return "brain.head.profile"
        case .reasoning: return "bubble.left.and.text.bubble.right"
        case .tool: return "wrench.and.screwdriver.fill"
        case .step: return "checkmark.circle.fill"
        case .artifact: return "doc.fill"
        case .pause: return "hand.raised.fill"
        }
    }

    private var statusColor: Color {
        if row.status == .failed { return theme.dangerColor }
        if row.status == .waiting { return theme.warningColor }
        if row.status == .done { return theme.successColor }
        switch row.tone {
        case .info: return theme.accentColor
        case .tool: return theme.warningColor
        case .error: return theme.dangerColor
        case .reasoning: return theme.discoveryColor
        case .pause: return theme.warningColor
        }
    }
}

private struct ActivityLogSheet: View {
    let rows: [ActivityRow]
    let sessionId: String?
    @ObservedObject var theme: ThemeManager
    let onPathAction: (String, ActivityPathAction) -> Void
    let onOpenURL: (URL) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var resultTarget: ActivityResultTarget?
    @State private var resultError: String?

    var body: some View {
        NavigationStack {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 12) {
                    ForEach(rows) { row in
                        ActivityTimelineRow(
                            row: row,
                            theme: theme,
                            onPathAction: onPathAction,
                            onOpenResult: openResult
                        )
                        Divider().overlay(theme.controlBorderColor)
                    }
                }
                .padding(16)
            }
            .background(theme.backgroundColor.ignoresSafeArea())
            .navigationTitle("Activity · \(rows.count) steps")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
            .environment(\.openURL, OpenURLAction { url in
                dismiss()
                onOpenURL(url)
                return .handled
            })
        }
        .preferredColorScheme(theme.colorScheme)
        .tint(theme.accentColor)
        // Present from the activity sheet itself. Dismissing this sheet and
        // asking its parent to present another one in the same run-loop turn
        // races UIKit's dismissal and can leave the new sheet with an empty
        // content host.
        .sheet(item: $resultTarget) { target in
            ActivityResultViewer(target: target, theme: theme)
        }
        .alert("Result unavailable", isPresented: Binding(
            get: { resultError != nil },
            set: { if !$0 { resultError = nil } }
        )) {
            Button("OK", role: .cancel) { resultError = nil }
        } message: {
            Text(resultError ?? "The complete result could not be opened.")
        }
    }

    private func openResult(_ row: ActivityRow) {
        guard let resultRef = row.resultRef else {
            resultError = "This complete result is no longer available."
            return
        }
        guard row.resultOwner != nil || row.taskId != nil || sessionId != nil else {
            resultError = "This complete result is no longer attached to a readable chat or task owner."
            return
        }
        resultTarget = ActivityResultTarget(
            sessionId: sessionId,
            resultRef: resultRef,
            title: row.label,
            contentHash: row.resultHash,
            sizeBytes: row.resultSizeBytes,
            owner: row.resultOwner,
            taskId: row.taskId,
            executionId: row.executionId
        )
    }
}

private struct ActivityStopRunButton: View {
    let executionId: String
    @ObservedObject var theme: ThemeManager
    @StateObject private var viewModel: ExecutionControlViewModel
    @ObservedObject private var coordinator = ExecutionControlCoordinator.shared
    @State private var confirmStop = false

    init(executionId: String, theme: ThemeManager) {
        self.executionId = executionId
        self.theme = theme
        _viewModel = StateObject(wrappedValue: ExecutionControlViewModel(executionId: executionId))
    }

    private var busy: Bool {
        coordinator.snapshot(for: executionId).busyAction != nil
    }

    var body: some View {
        Button { confirmStop = true } label: {
            Group {
                if busy { ProgressView().controlSize(.mini) }
                else { Image(systemName: "stop.fill").font(.system(size: 10, weight: .bold)) }
            }
            .foregroundColor(theme.dangerColor)
            .frame(width: 28, height: 26)
            .background(theme.surfaceColor)
            .clipShape(Circle())
            .overlay(Circle().stroke(theme.dangerColor.opacity(0.3), lineWidth: 1))
        }
        .buttonStyle(.plain)
        .disabled(busy)
        .accessibilityLabel("Stop run")
        .confirmationDialog("Stop this run?", isPresented: $confirmStop, titleVisibility: .visible) {
            Button("Stop run", role: .destructive) { viewModel.perform(.cancel) }
            Button("Keep running", role: .cancel) {}
        } message: {
            Text("The active execution and its running children will be cancelled.")
        }
        .alert("Could not stop run", isPresented: Binding(
            get: { viewModel.errorMessage != nil },
            set: { if !$0 { viewModel.errorMessage = nil } }
        )) {
            Button("OK", role: .cancel) { viewModel.errorMessage = nil }
        } message: {
            Text(viewModel.errorMessage ?? "The run could not be stopped.")
        }
    }
}

private struct ActivityResultTarget: Identifiable {
    let sessionId: String?
    let resultRef: String
    let title: String
    let contentHash: String?
    let sizeBytes: Int?
    let owner: ActivityResultOwner?
    let taskId: String?
    let executionId: String?
    var id: String { resultRef }
}

private struct ActivityResultViewer: View {
    let target: ActivityResultTarget
    @ObservedObject var theme: ThemeManager
    @Environment(\.dismiss) private var dismiss
    @State private var text = ""
    @State private var verifiedHash: String?
    @State private var loading = true
    @State private var errorMessage: String?

    var body: some View {
        NavigationStack {
            Group {
                if loading {
                    ProgressView("Loading complete result…")
                } else if let errorMessage {
                    ContentUnavailableView(
                        "Result unavailable",
                        systemImage: "exclamationmark.shield",
                        description: Text(errorMessage)
                    )
                } else {
                    // A two-axis ScrollView gives selectable Text no bounded
                    // layout proposal on iPhone. The reconstructed string was
                    // present, but the host could measure it as an empty view.
                    // Wrap to the sheet width and scroll vertically, as Web's
                    // result panel does at a phone-sized viewport.
                    ScrollView(.vertical) {
                        Text(verbatim: text)
                            .font(.themedMono(12))
                            .foregroundColor(theme.textColor)
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .fixedSize(horizontal: false, vertical: true)
                            .padding(16)
                    }
                }
            }
            .background(theme.backgroundColor.ignoresSafeArea())
            .navigationTitle(target.title)
            .navigationBarTitleDisplayMode(.inline)
            .safeAreaInset(edge: .bottom) {
                if let hash = verifiedHash ?? target.contentHash {
                    Text("Verified content · \(String(hash.prefix(12)))")
                        .font(.themedMono(10))
                        .foregroundColor(theme.secondaryTextColor)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 7)
                        .background(theme.surfaceColor)
                }
            }
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
        .preferredColorScheme(theme.colorScheme)
        .tint(theme.accentColor)
        .task(id: target.resultRef) {
            loading = true
            errorMessage = nil
            do {
                let result = try await ActivityResultClient.readAll(
                    sessionId: target.sessionId,
                    resultRef: target.resultRef,
                    owner: target.owner,
                    taskId: target.taskId,
                    executionId: target.executionId,
                    expectedContentHash: target.contentHash
                )
                text = result.text
                verifiedHash = result.contentHash
            } catch {
                errorMessage = error.localizedDescription
            }
            loading = false
        }
    }
}

enum ActivityResultClient {
    struct CompleteResult {
        let text: String
        let contentHash: String?
    }

    private struct FragmentAssembly {
        var nextByte: Int
        let totalBytes: Int
        var text: String
    }

    private static func pointerTokens(_ path: String) throws -> [String] {
        if path.isEmpty { return [] }
        guard path.first == "/" else {
            throw NSError(
                domain: "ActivityResultClient",
                code: 2,
                userInfo: [NSLocalizedDescriptionKey: "The result contained an invalid reconstruction path."]
            )
        }
        var tokens: [String] = []
        for encoded in path.dropFirst().split(separator: "/", omittingEmptySubsequences: false) {
            var awaitingEscape = false
            for character in encoded {
                if awaitingEscape {
                    guard character == "0" || character == "1" else {
                        throw NSError(
                            domain: "ActivityResultClient",
                            code: 12,
                            userInfo: [NSLocalizedDescriptionKey: "The result contained an invalid JSON-pointer escape."]
                        )
                    }
                    awaitingEscape = false
                } else if character == "~" {
                    awaitingEscape = true
                }
            }
            guard !awaitingEscape else {
                throw NSError(
                    domain: "ActivityResultClient",
                    code: 12,
                    userInfo: [NSLocalizedDescriptionKey: "The result contained an invalid JSON-pointer escape."]
                )
            }
            tokens.append(
                String(encoded).replacingOccurrences(of: "~1", with: "/")
                    .replacingOccurrences(of: "~0", with: "~")
            )
        }
        return tokens
    }

    private static func exactArrayIndex(_ token: String) -> Int? {
        guard token == "0" || (token.first != "0" && token.allSatisfy(\.isNumber)),
              let value = Int(token), value >= 0 else { return nil }
        return value
    }

    private static func isTokenPrefix(_ prefix: [String], of value: [String]) -> Bool {
        guard prefix.count <= value.count else { return false }
        return zip(prefix, value).allSatisfy { $0.0 == $0.1 }
    }

    private static func assertCompatiblePath(
        _ path: String,
        assignedPaths: [String: String],
        fragmentPaths: [String],
        continuingFragment: Bool = false
    ) throws {
        let tokens = try pointerTokens(path)
        for (assignedPath, assignedKind) in assignedPaths {
            let existing = try pointerTokens(assignedPath)
            if assignedPath == path {
                throw NSError(
                    domain: "ActivityResultClient",
                    code: 14,
                    userInfo: [NSLocalizedDescriptionKey: "The result contained a duplicate reconstruction path."]
                )
            }
            if isTokenPrefix(existing, of: tokens), assignedKind != "container" {
                throw NSError(
                    domain: "ActivityResultClient",
                    code: 25,
                    userInfo: [NSLocalizedDescriptionKey: "The result contained overlapping scalar reconstruction paths."]
                )
            }
            if isTokenPrefix(tokens, of: existing) {
                throw NSError(
                    domain: "ActivityResultClient",
                    code: 26,
                    userInfo: [NSLocalizedDescriptionKey: "The result contained an out-of-order parent reconstruction path."]
                )
            }
        }
        for fragmentPath in fragmentPaths {
            if fragmentPath == path, continuingFragment { continue }
            let existing = try pointerTokens(fragmentPath)
            if fragmentPath == path
                || isTokenPrefix(existing, of: tokens)
                || isTokenPrefix(tokens, of: existing) {
                throw NSError(
                    domain: "ActivityResultClient",
                    code: 27,
                    userInfo: [NSLocalizedDescriptionKey: "The result contained overlapping string-fragment reconstruction paths."]
                )
            }
        }
    }

    private static func assigning(_ root: Any?, tokens: ArraySlice<String>, value: Any) throws -> Any {
        guard let token = tokens.first else { return value }
        let remainder = tokens.dropFirst()
        let container: Any
        if let root {
            container = root
        } else {
            container = exactArrayIndex(token) == nil ? [String: Any]() : [Any]()
        }

        if var array = container as? [Any] {
            guard let index = exactArrayIndex(token) else {
                throw NSError(
                    domain: "ActivityResultClient",
                    code: 3,
                    userInfo: [NSLocalizedDescriptionKey: "The result contained a non-numeric array path."]
                )
            }
            guard index <= array.count else {
                throw NSError(
                    domain: "ActivityResultClient",
                    code: 13,
                    userInfo: [NSLocalizedDescriptionKey: "The result array entries were missing or out of order."]
                )
            }
            if index == array.count { array.append(NSNull()) }
            let child: Any? = array[index] is NSNull ? nil : array[index]
            array[index] = try assigning(child, tokens: remainder, value: value)
            return array
        }
        if var object = container as? [String: Any] {
            object[token] = try assigning(object[token], tokens: remainder, value: value)
            return object
        }
        throw NSError(
            domain: "ActivityResultClient",
            code: 4,
            userInfo: [NSLocalizedDescriptionKey: "The result contained overlapping scalar reconstruction paths."]
        )
    }

    private static func reconstruct(entries: [[String: Any]], version: Int) throws -> Any {
        guard version == 0 || version == 1 else {
            throw NSError(
                domain: "ActivityResultClient",
                code: 5,
                userInfo: [NSLocalizedDescriptionKey: "This result uses an unsupported reconstruction version."]
            )
        }
        var root: Any?
        var fragments: [String: FragmentAssembly] = [:]
        var assignedPaths: [String: String] = [:]
        for entry in entries {
            let fieldPath = entry["field_path"] as? String ?? ""
            let sourceIndex = (entry["source_index"] as? NSNumber)?.intValue
            let path = entry["reconstruction_path"] as? String
                ?? sourceIndex.map { "\(fieldPath)/\($0)" }
                ?? fieldPath
            let kind = entry["kind"] as? String ?? "complete_value"
            if kind == "string_fragment" {
                guard version == 1,
                      let text = entry["value"] as? String,
                      let metadata = entry["string_fragment"] as? [String: Any],
                      let byteStart = (metadata["byte_start"] as? NSNumber)?.intValue,
                      let byteEnd = (metadata["byte_end"] as? NSNumber)?.intValue,
                      let totalBytes = (metadata["total_bytes"] as? NSNumber)?.intValue else {
                    throw NSError(
                        domain: "ActivityResultClient",
                        code: 6,
                        userInfo: [NSLocalizedDescriptionKey: "The result contained an invalid string fragment."]
                    )
                }
                try assertCompatiblePath(
                    path,
                    assignedPaths: assignedPaths,
                    fragmentPaths: Array(fragments.keys),
                    continuingFragment: fragments[path] != nil
                )
                var state = fragments[path] ?? FragmentAssembly(nextByte: 0, totalBytes: totalBytes, text: "")
                guard state.totalBytes == totalBytes,
                      state.nextByte == byteStart,
                      byteEnd == byteStart + text.utf8.count,
                      byteEnd <= totalBytes else {
                    throw NSError(
                        domain: "ActivityResultClient",
                        code: 7,
                        userInfo: [NSLocalizedDescriptionKey: "The result string fragments were missing, duplicated, or out of order."]
                    )
                }
                state.text += text
                state.nextByte = byteEnd
                if byteEnd == totalBytes {
                    root = try assigning(root, tokens: pointerTokens(path)[...], value: state.text)
                    fragments.removeValue(forKey: path)
                    assignedPaths[path] = "string_fragment"
                } else {
                    fragments[path] = state
                }
                continue
            }
            guard kind == "complete_value" || (kind == "container" && version == 1),
                  let value = entry["value"] else {
                throw NSError(
                    domain: "ActivityResultClient",
                    code: 8,
                    userInfo: [NSLocalizedDescriptionKey: "The result contained an unknown reconstruction unit."]
                )
            }
            if kind == "container", !(value is [String: Any]) && !(value is [Any]) {
                throw NSError(
                    domain: "ActivityResultClient",
                    code: 9,
                    userInfo: [NSLocalizedDescriptionKey: "The result container was malformed."]
                )
            }
            if kind == "container" {
                let objectIsNonempty = (value as? [String: Any])?.isEmpty == false
                let arrayIsNonempty = (value as? [Any])?.isEmpty == false
                guard !objectIsNonempty && !arrayIsNonempty else {
                    throw NSError(
                        domain: "ActivityResultClient",
                        code: 15,
                        userInfo: [NSLocalizedDescriptionKey: "The result container entry was not empty."]
                    )
                }
            }
            try assertCompatiblePath(
                path,
                assignedPaths: assignedPaths,
                fragmentPaths: Array(fragments.keys)
            )
            root = try assigning(root, tokens: pointerTokens(path)[...], value: value)
            assignedPaths[path] = kind
        }
        guard fragments.isEmpty, let root else {
            throw NSError(
                domain: "ActivityResultClient",
                code: 10,
                userInfo: [NSLocalizedDescriptionKey: "The result ended before all complete values arrived."]
            )
        }
        return root
    }

    static func readAll(
        sessionId: String?,
        resultRef: String,
        owner: ActivityResultOwner? = nil,
        taskId: String? = nil,
        executionId: String? = nil,
        expectedContentHash: String? = nil,
        session: URLSession = .shared
    ) async throws -> CompleteResult {
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "-._~"))
        let path: String
        let boundExecutionId: String?
        switch owner {
        case .chat(let ownerSessionId):
            guard let encodedSession = ownerSessionId.addingPercentEncoding(withAllowedCharacters: allowed) else {
                throw URLError(.badURL)
            }
            path = "api/magician/v2/chat/sessions/\(encodedSession)/results/read"
            boundExecutionId = nil
        case .task(let ownerTaskId, let ownerExecutionId):
            guard let encodedTask = ownerTaskId.addingPercentEncoding(withAllowedCharacters: allowed) else {
                throw URLError(.badURL)
            }
            path = "api/magician/v3/tasks/\(encodedTask)/results/read"
            boundExecutionId = ownerExecutionId
        case .ephemeralVoice:
            throw NSError(
                domain: "ActivityResultClient",
                code: 25,
                userInfo: [NSLocalizedDescriptionKey: "This voice result is no longer attached to a readable chat session."]
            )
        case nil:
            // Compatibility for activity persisted before result-owner
            // metadata. New events never infer ownership from navigation ids.
            if let taskId, !taskId.isEmpty,
               let encodedTask = taskId.addingPercentEncoding(withAllowedCharacters: allowed) {
                path = "api/magician/v3/tasks/\(encodedTask)/results/read"
                boundExecutionId = executionId
            } else if let sessionId, !sessionId.isEmpty,
                      let encodedSession = sessionId.addingPercentEncoding(withAllowedCharacters: allowed) {
                path = "api/magician/v2/chat/sessions/\(encodedSession)/results/read"
                boundExecutionId = nil
            } else {
                throw URLError(.badURL)
            }
        }
        guard let url = URL(string: "\(MagicianAccess.baseURL.absoluteString)/\(path)") else {
            throw URLError(.badURL)
        }
        var cursor: String?
        var allEntries: [[String: Any]] = []
        var reconstructionVersion: Int?
        var contentHash: String?
        var expectedPageStart = 0
        var expectedTotalEntries: Int?
        var seenCursors = Set<String>()
        for pageIndex in 0..<256 {
            var request = URLRequest(url: url)
            request.httpMethod = "POST"
            request.timeoutInterval = 30
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            var payload: [String: Any] = [
                "result_ref": resultRef,
                "field_paths": [],
                "max_records": 100
            ]
            if let cursor { payload["cursor"] = cursor }
            if let boundExecutionId, !boundExecutionId.isEmpty {
                payload["execution_id"] = boundExecutionId
            }
            request.httpBody = try JSONSerialization.data(withJSONObject: payload)
            MagicianAccess.authorize(&request)
            let (data, response) = try await session.data(for: request)
            guard let http = response as? HTTPURLResponse,
                  (200..<300).contains(http.statusCode),
                  let envelope = try JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let page = envelope["page"] as? [String: Any] else {
                let error = (try? JSONSerialization.jsonObject(with: data) as? [String: Any])?["error"] as? String
                throw NSError(
                    domain: "ActivityResultClient",
                    code: (response as? HTTPURLResponse)?.statusCode ?? -1,
                    userInfo: [NSLocalizedDescriptionKey: error ?? "The complete result could not be read."]
                )
            }
            guard let pageHash = page["content_hash"] as? String, !pageHash.isEmpty else {
                throw NSError(
                    domain: "ActivityResultClient",
                    code: 16,
                    userInfo: [NSLocalizedDescriptionKey: "The complete result was missing its verified content hash."]
                )
            }
            if contentHash == nil,
               let expectedContentHash,
               !expectedContentHash.isEmpty,
               expectedContentHash != pageHash {
                throw NSError(
                    domain: "ActivityResultClient",
                    code: 24,
                    userInfo: [NSLocalizedDescriptionKey: "The complete result did not match the projected content hash."]
                )
            }
            if let contentHash, contentHash != pageHash {
                throw NSError(
                    domain: "ActivityResultClient",
                    code: 17,
                    userInfo: [NSLocalizedDescriptionKey: "The complete result content hash changed between pages."]
                )
            }
            contentHash = pageHash
            if let pageReference = page["content_ref"] as? String,
               pageReference != resultRef {
                throw NSError(
                    domain: "ActivityResultClient",
                    code: 18,
                    userInfo: [NSLocalizedDescriptionKey: "The complete result identity changed between pages."]
                )
            }
            let pageVersion = (page["reconstruction_version"] as? NSNumber)?.intValue ?? 0
            if let reconstructionVersion, reconstructionVersion != pageVersion {
                throw NSError(
                    domain: "ActivityResultClient",
                    code: 11,
                    userInfo: [NSLocalizedDescriptionKey: "The result changed reconstruction version between pages."]
                )
            }
            reconstructionVersion = pageVersion
            let entries = page["entries"] as? [[String: Any]] ?? []
            if let pageStart = (page["page_start"] as? NSNumber)?.intValue,
               pageStart != expectedPageStart {
                throw NSError(
                    domain: "ActivityResultClient",
                    code: 19,
                    userInfo: [NSLocalizedDescriptionKey: "The complete result pages were missing, duplicated, or out of order."]
                )
            }
            let pageTotal = (page["total_entries"] as? NSNumber)?.intValue
                ?? (page["total_records"] as? NSNumber)?.intValue
            if let pageTotal {
                guard pageTotal >= 0, expectedTotalEntries == nil || expectedTotalEntries == pageTotal else {
                    throw NSError(
                        domain: "ActivityResultClient",
                        code: 20,
                        userInfo: [NSLocalizedDescriptionKey: "The complete result entry count changed between pages."]
                    )
                }
                expectedTotalEntries = pageTotal
            }
            if cursor != nil, entries.isEmpty {
                throw NSError(
                    domain: "ActivityResultClient",
                    code: 21,
                    userInfo: [NSLocalizedDescriptionKey: "The complete result cursor made no forward progress."]
                )
            }
            allEntries.append(contentsOf: entries)
            expectedPageStart += entries.count
            cursor = page["next_cursor"] as? String
            if let cursor, !seenCursors.insert(cursor).inserted {
                throw NSError(
                    domain: "ActivityResultClient",
                    code: 22,
                    userInfo: [NSLocalizedDescriptionKey: "The complete result repeated a page cursor."]
                )
            }
            if cursor == nil {
                if let expectedTotalEntries, expectedTotalEntries != allEntries.count {
                    throw NSError(
                        domain: "ActivityResultClient",
                        code: 23,
                        userInfo: [NSLocalizedDescriptionKey: "The complete result ended before every entry was returned."]
                    )
                }
                break
            }
            if pageIndex == 255 {
                throw NSError(
                    domain: "ActivityResultClient",
                    code: 1,
                    userInfo: [NSLocalizedDescriptionKey: "The result exceeded the safe page limit."]
                )
            }
        }
        let value = try reconstruct(entries: allEntries, version: reconstructionVersion ?? 0)
        // `withoutEscapingSlashes` matters because this text is *shown to the
        // user*, not re-parsed. `JSONSerialization` escapes `/` as `\/` by
        // default, which is legal JSON and an artefact of the serialiser rather
        // than of the result — so without it every path, URL and filename in
        // every tool result renders with backslashes through it, and a JSON
        // Pointer key that decoded a `~1` back to `/` reads as though the slash
        // were escaped in the data.
        let pretty = try JSONSerialization.data(
            withJSONObject: value,
            options: [.prettyPrinted, .sortedKeys, .fragmentsAllowed, .withoutEscapingSlashes]
        )
        guard let text = String(data: pretty, encoding: .utf8), !text.isEmpty else {
            throw NSError(
                domain: "ActivityResultClient",
                code: 28,
                userInfo: [NSLocalizedDescriptionKey: "The complete result decoded to empty text."]
            )
        }
        return CompleteResult(text: text, contentHash: contentHash)
    }
}

enum ActivityPathAction: String { case file, folder }

enum ActivityPathClient {
    static func perform(
        sessionId: String,
        absolutePath: String,
        action: ActivityPathAction,
        session: URLSession = .shared
    ) async throws {
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "-._~"))
        guard let encodedSession = sessionId.addingPercentEncoding(withAllowedCharacters: allowed),
              let url = URL(string:
                "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/chat/sessions/\(encodedSession)/outputs/open-\(action.rawValue)"
              ) else { throw URLError(.badURL) }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.timeoutInterval = 15
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONSerialization.data(withJSONObject: ["absolute_path": absolutePath])
        MagicianAccess.authorize(&request)
        let (_, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
            throw URLError(.badServerResponse)
        }
    }
}
