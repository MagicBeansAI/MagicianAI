import SwiftUI
import UIKit
import MarkdownUI

// MARK: - Act card

/// One act card: a 44 pt header (title, its one-line summary while closed —
/// the Plan act keeps its summary open), and the act's body while open.
struct TaskDetailActCard: View {
    let act: ActId
    let snapshot: TaskDetailSnapshot
    let isOpen: Bool
    let taskId: String
    @ObservedObject var model: TaskDetailScreenModel
    let onToggle: () -> Void
    let onNotice: (String) -> Void

    @ObservedObject private var theme = ThemeManager.shared
    @State private var planDocument = false
    @Environment(\.openPlanInspector) private var openPlanInspector
    /// Fallback when no `planInspectorHost` is above this card.
    @State private var planInspectorSheet: PlanInspectorRoute?

    private var title: String {
        switch act {
        case .output: return "Output"
        case .run: return "Run"
        case .plan: return "Plan"
        }
    }

    private var icon: String {
        switch act {
        case .output: return "shippingbox"
        case .run: return "bolt.horizontal"
        case .plan: return "list.bullet.clipboard"
        }
    }

    var body: some View {
        let summary = TaskDetailLayout.summary(for: act, snapshot, now: Date())
        let showsSummary = !summary.isEmpty && (!isOpen || TaskDetailLayout.keepsSummaryWhenOpen(act))
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .center, spacing: 8) {
                Button(action: onToggle) {
                    HStack(alignment: .center, spacing: 10) {
                        Image(systemName: icon)
                            .font(.system(size: 15, weight: .semibold))
                            .foregroundColor(theme.accentColor)
                            .frame(width: 22)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(title)
                                .font(.themed(16, weight: .bold))
                                .foregroundColor(theme.textColor)
                            if showsSummary {
                                Text(summary)
                                    .font(.themed(13))
                                    .foregroundColor(theme.secondaryTextColor)
                                    .lineLimit(1)
                                    .truncationMode(.tail)
                            }
                        }
                        Spacer(minLength: 4)
                        Image(systemName: "chevron.down")
                            .font(.system(size: 12, weight: .semibold))
                            .foregroundColor(theme.secondaryTextColor)
                            .rotationEffect(.degrees(isOpen ? 0 : -90))
                    }
                    .frame(minHeight: Lively.tapMin)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(showsSummary ? "\(title), \(summary)" : title)
                .accessibilityHint(isOpen ? "Collapse" : "Expand")
                .accessibilityIdentifier("task-detail-act-\(act.rawValue)")

                // A plan with a graph opens the Plan Inspector (its More
                // menu keeps the plan document); a document-only plan opens
                // the document.
                if act == .plan, !snapshot.planGraphSteps.isEmpty {
                    Button {
                        let route = PlanInspectorRoute(taskId: taskId, title: snapshot.title,
                                                       planDocument: snapshot.planMarkdown)
                        if let openPlanInspector { openPlanInspector(route) } else { planInspectorSheet = route }
                    } label: {
                        Label("Plan graph", systemImage: "point.3.connected.trianglepath.dotted")
                    }
                    .buttonStyle(ChipStyle())
                    .accessibilityIdentifier("task-detail-plan-graph")
                } else if act == .plan, let markdown = snapshot.planMarkdown, !markdown.isEmpty {
                    Button { planDocument = true } label: {
                        Label("Plan document", systemImage: "doc.text")
                    }
                    .buttonStyle(ChipStyle())
                    .accessibilityIdentifier("task-detail-plan-document")
                }
            }
            if isOpen {
                Divider().overlay(theme.cardBorderColor).padding(.vertical, 8)
                actBody
                    .transition(.opacity)
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 6)
        .padding(.bottom, isOpen ? 8 : 0)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(theme.cardColor)
        .clipShape(RoundedRectangle(cornerRadius: Lively.cardRadius, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: Lively.cardRadius, style: .continuous)
                .strokeBorder(theme.cardBorderColor, lineWidth: 1))
        .fullScreenCover(item: $planInspectorSheet) { PlanInspectorSheetHost(route: $0) }
        .sheet(isPresented: $planDocument) {
            NavigationStack {
                ScrollView {
                    Markdown(snapshot.planMarkdown ?? "")
                        .markdownTextStyle { ForegroundColor(theme.textColor) }
                        .textSelection(.enabled)
                        .padding()
                }
                .background(theme.washGradient.ignoresSafeArea())
                .navigationTitle("Plan document")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { planDocument = false } } }
            }
        }
    }

    @ViewBuilder
    private var actBody: some View {
        switch act {
        case .output: TaskDetailOutputBody(snapshot: snapshot, taskId: taskId, model: model, onNotice: onNotice)
        case .run: TaskDetailRunBody(snapshot: snapshot, model: model)
        case .plan: TaskDetailPlanBody(snapshot: snapshot, model: model, onNotice: onNotice)
        }
    }
}

// MARK: - Output

struct TaskDetailOutputBody: View {
    let snapshot: TaskDetailSnapshot
    let taskId: String
    @ObservedObject var model: TaskDetailScreenModel
    let onNotice: (String) -> Void
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        let files = TaskDetailLayout.outputFiles(snapshot)
        let previewed = TaskDetailLayout.defaultPreview(files)
        VStack(alignment: .leading, spacing: 12) {
            if let summary = snapshot.resultSummary {
                Markdown(summary)
                    .markdownTextStyle { ForegroundColor(theme.textColor) }
                    .textSelection(.enabled)
            }
            if snapshot.synthesisPending {
                TaskDetailNotice(text: "The run finished; its output is still being written up.",
                                 tint: theme.accentColor, icon: "wand.and.stars")
            } else if snapshot.synthesisFailedExecutionId != nil {
                TaskDetailNotice(text: "Writing up the output failed. Retry it from the ⋯ menu.",
                                 tint: theme.dangerColor, icon: "exclamationmark.arrow.triangle.2.circlepath")
            }
            if files.isEmpty {
                Text(snapshot.resultSummary == nil ? "No output yet." : "No files were delivered.")
                    .font(.themed(14))
                    .foregroundColor(theme.secondaryTextColor)
            }
            ForEach(files) { file in
                TaskOutputFileRow(artifact: file, taskId: taskId, showsPreview: file.id == previewed?.id,
                                  onNotice: onNotice)
                if file.id != files.last?.id { Divider().overlay(theme.cardBorderColor) }
            }
        }
        .accessibilityIdentifier("task-detail-output-body")
    }
}

/// One deliverable. Line 1: the name (middle ellipsis; a tap copies the
/// absolute path) and `type · size`. Line 2: Open · Share · Copy path. The
/// previewed file keeps its row; its preview sits under it, small.
struct TaskOutputFileRow: View {
    let artifact: TaskDetailArtifact
    let taskId: String
    var showsPreview = false
    let onNotice: (String) -> Void

    @ObservedObject private var theme = ThemeManager.shared
    @StateObject private var file = TaskOutputFile()
    @State private var opened: ArtifactRef?
    @State private var sharePayload: SharePayload?

    private var ref: ArtifactRef? {
        guard !taskId.hasPrefix("agent-cycle:") else { return nil }
        return ArtifactRef.taskOutput(taskId: taskId, relativePath: artifact.relativePath, mime: artifact.mediaType)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                CopyOnTap(value: TaskDetailLayout.copyPath(artifact), kind: .path, onCopied: onNotice) { copied in
                    CopiedSwap(copied: copied, font: .themed(15, weight: .semibold)) {
                        Text(artifact.displayName)
                            .font(.themed(15, weight: .semibold))
                            .foregroundColor(theme.textColor)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    .contentShape(Rectangle().inset(by: -8))
                }
                .buttonStyle(.plain)
                .accessibilityLabel(artifact.displayName)
                Spacer(minLength: 6)
                Text(TaskDetailLayout.fileMeta(artifact))
                    .font(.themed(12))
                    .foregroundColor(theme.secondaryTextColor)
                    .lineLimit(1)
                    .layoutPriority(1)
            }
            .frame(minHeight: 30)
            HStack(spacing: 6) {
                if let ref {
                    Button { opened = ref } label: { Label("Open", systemImage: "eye") }
                        .buttonStyle(ChipStyle())
                    shareChip
                }
                CopyOnTap(value: TaskDetailLayout.copyPath(artifact), kind: .path, onCopied: onNotice) { copied in
                    CopiedSwap(copied: copied, font: .themed(ChipStyle.fontSize, weight: .semibold)) {
                        Label("Copy path", systemImage: "doc.on.doc")
                    }
                }
                .buttonStyle(ChipStyle())
            }
            if showsPreview, let ref {
                TaskOutputPreview(artifact: artifact, ref: ref, file: file)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("task-detail-output-\(artifact.displayName)")
        .sheet(item: $opened) { ArtifactViewer(artifact: $0) }
        .sheet(item: $sharePayload) { ShareSheet(items: $0.items) }
    }

    /// Share the file itself: a `ShareLink` once it is on the device, else
    /// a chip that downloads it and then shares it.
    @ViewBuilder
    private var shareChip: some View {
        if let local = file.localURL {
            ShareLink(item: local) { Label("Share", systemImage: "square.and.arrow.up") }
                .buttonStyle(ChipStyle())
        } else {
            Button {
                Task {
                    guard let ref else { return }
                    if let local = await file.download(ref) { sharePayload = SharePayload(items: [local]) }
                    else { onNotice("The file could not be downloaded.") }
                }
            } label: {
                if file.isDownloading {
                    ProgressView().controlSize(.mini)
                } else {
                    Label("Share", systemImage: "square.and.arrow.up")
                }
            }
            .buttonStyle(ChipStyle())
        }
    }
}

/// A downloaded copy of one output file (for Share and the preview).
@MainActor
final class TaskOutputFile: ObservableObject {
    @Published private(set) var localURL: URL?
    @Published private(set) var isDownloading = false

    func download(_ ref: ArtifactRef) async -> URL? {
        if let localURL { return localURL }
        guard let url = ref.url else { return nil }
        isDownloading = true
        defer { isDownloading = false }
        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        guard let (temp, response) = try? await URLSession.shared.download(for: request),
              let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else { return nil }
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("task-outputs", isDirectory: true)
        try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let dest = folder.appendingPathComponent(ref.filename)
        try? FileManager.default.removeItem(at: dest)
        guard (try? FileManager.default.moveItem(at: temp, to: dest)) != nil else { return nil }
        localURL = dest
        return dest
    }
}

/// The previewed file, small: an image, or the first lines of text. The
/// file's own snippet shows at once; the downloaded text replaces it.
struct TaskOutputPreview: View {
    let artifact: TaskDetailArtifact
    let ref: ArtifactRef
    @ObservedObject var file: TaskOutputFile
    @ObservedObject private var theme = ThemeManager.shared
    @State private var text: String?

    private static let lineCap = 24

    var body: some View {
        Group {
            if case .image = ref.kind, let url = ref.url {
                AuthAsyncImage(urlString: url.absoluteString, maxHeight: 220)
                    .clipShape(RoundedRectangle(cornerRadius: 8))
            } else if let shown = text ?? artifact.bodySnippet {
                Text(shown)
                    .font(.themedMono(11))
                    .foregroundColor(theme.textColor)
                    .lineLimit(Self.lineCap)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(10)
                    .background(theme.controlColor)
                    .clipShape(RoundedRectangle(cornerRadius: 8))
                    .textSelection(.enabled)
            } else {
                ProgressView().controlSize(.small).frame(maxWidth: .infinity).padding(8)
            }
        }
        .accessibilityIdentifier("task-detail-preview")
        .task {
            guard case .text = ref.kind, text == nil,
                  let local = await file.download(ref),
                  let data = try? Data(contentsOf: local) else { return }
            let decoded = String(decoding: data.prefix(16_000), as: UTF8.self)
            text = decoded.split(separator: "\n", omittingEmptySubsequences: false)
                .prefix(Self.lineCap).joined(separator: "\n")
        }
    }
}

// MARK: - Run

struct TaskDetailRunBody: View {
    let snapshot: TaskDetailSnapshot
    @ObservedObject var model: TaskDetailScreenModel
    @ObservedObject private var theme = ThemeManager.shared
    @State private var showAllEvents = false
    /// Plan steps whose attributed activity is open, by step id.
    @State private var openSteps: Set<String> = []
    @State private var offPlanOpen = false

    private static let eventPreview = 30

    var body: some View {
        let events = snapshot.activity
        let shown = showAllEvents ? TaskTimelineFormatting.latest(events) : Array(events.suffix(Self.eventPreview))
        VStack(alignment: .leading, spacing: 12) {
            if let selected = model.selectedExecutionId,
               let latest = snapshot.history.first?.id, selected != latest {
                HStack(spacing: 6) {
                    Text("Showing run \(TaskDetailScreen.shortId(selected))")
                        .font(.themed(12))
                        .foregroundColor(theme.secondaryTextColor)
                    Button("Latest run") { model.selectExecution(latest) }
                        .buttonStyle(ChipStyle())
                }
            }
            if let error = snapshot.selectedExecutionError, !error.isEmpty {
                TaskDetailNotice(text: error, tint: theme.dangerColor, icon: "exclamationmark.triangle.fill")
            }
            if !snapshot.planStepRows.isEmpty {
                let groups = PlanStepActivity.group(events)
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(snapshot.planStepRows.enumerated()), id: \.element.id) { index, row in
                        if index > 0 { StepHairline(leadingInset: PlanStepCheckRow.textInset) }
                        let stepRows = row.stepId.flatMap { groups.byStep[$0] } ?? []
                        let isOpen = row.stepId.map { openSteps.contains($0) } ?? false
                        PlanStepCheckRow(
                            row: row,
                            activityCount: groups.count(for: row.stepId),
                            expanded: isOpen,
                            onToggle: row.stepId.map { stepId in
                                { toggle(stepId) }
                            })
                        if isOpen, !stepRows.isEmpty {
                            attributedRows(stepRows)
                        }
                    }
                    if groups.attributed, !groups.offPlan.isEmpty {
                        StepHairline()
                        offPlanGroup(groups.offPlan)
                    }
                }
                .accessibilityIdentifier("task-detail-plan-steps")
            }
            if let summary = snapshot.runSummary, !summary.isEmpty {
                Markdown(summary)
                    .markdownTextStyle { ForegroundColor(theme.textColor) }
                    .textSelection(.enabled)
            }
            if events.isEmpty {
                Text(snapshot.selectedExecutionId == nil ? "Not started yet." : "No activity recorded yet.")
                    .font(.themed(14))
                    .foregroundColor(theme.secondaryTextColor)
            } else {
                Text(timelineLabel(events.count))
                    .font(.themed(12, weight: .semibold))
                    .foregroundColor(theme.secondaryTextColor)
                    .padding(.top, snapshot.planStepRows.isEmpty ? 0 : 4)
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(shown) { event in
                        TaskRunEventRow(
                            event: event,
                            origin: snapshot.timelineOrigin,
                            delegatedAgent: TaskDelegationTimeline.delegatedAgent(
                                for: event, delegations: snapshot.delegations),
                            isFirst: event.id == shown.first?.id,
                            isLast: event.id == shown.last?.id)
                    }
                }
                if events.count > shown.count {
                    Button("Show all \(min(events.count, TaskTimelineFormatting.renderLimit)) events") {
                        showAllEvents = true
                    }
                    .buttonStyle(ChipStyle())
                }
            }
        }
    }

    private func timelineLabel(_ count: Int) -> String {
        let duration = TaskTimelineFormatting.duration(seconds: snapshot.runDuration(at: Date()))
        // With off-plan attribution the steps above hold the grouped view, so
        // this flat list is all of the run's activity.
        let title = PlanStepActivity.group(snapshot.activity).attributed ? "All activity" : "Timeline"
        return [title, "\(count) \(count == 1 ? "event" : "events")", duration]
            .compactMap { $0 }.joined(separator: " · ")
    }

    private func toggle(_ stepId: String) {
        if openSteps.contains(stepId) { openSteps.remove(stepId) } else { openSteps.insert(stepId) }
    }

    /// A step's (or the off-plan group's) activity, indented under it.
    private func attributedRows(_ rows: [TaskDetailActivity]) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(rows) { event in
                TaskRunEventRow(
                    event: event,
                    origin: snapshot.timelineOrigin,
                    delegatedAgent: TaskDelegationTimeline.delegatedAgent(
                        for: event, delegations: snapshot.delegations),
                    isFirst: event.id == rows.first?.id,
                    isLast: event.id == rows.last?.id)
            }
        }
        .padding(.leading, PlanStepCheckRow.textInset)
        .padding(.bottom, 4)
    }

    /// "Outside the plan · N": muted, folded, explained.
    private func offPlanGroup(_ rows: [TaskDetailActivity]) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Button {
                offPlanOpen.toggle()
            } label: {
                HStack(alignment: .firstTextBaseline, spacing: 10) {
                    Image(systemName: offPlanOpen ? "chevron.down" : "chevron.right")
                        .font(.system(size: 10, weight: .semibold))
                        .frame(width: 18)
                    Text(PlanStepActivity.offPlanLabel(rows.count))
                        .font(.themed(Lively.stepTitleSize))
                    Spacer(minLength: 0)
                }
                .foregroundColor(theme.secondaryTextColor)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .padding(.top, Lively.stepRowPadding)
            .accessibilityLabel(PlanStepActivity.offPlanLabel(rows.count))
            .accessibilityHint(PlanStepActivity.offPlanExplanation)
            .accessibilityIdentifier("task-detail-off-plan")
            Text(PlanStepActivity.offPlanExplanation)
                .font(.themed(Lively.stepMetaSize))
                .foregroundColor(theme.secondaryTextColor)
                .padding(.leading, PlanStepCheckRow.textInset)
                .padding(.bottom, offPlanOpen ? 2 : Lively.stepRowPadding)
            if offPlanOpen {
                attributedRows(rows)
            }
        }
        .opacity(0.85)
    }
}

/// One plan step in the Run checklist: ✓ done, ✕ failed, a breathing dot in
/// progress, ○ pending, ↷ skipped, and "not reached" after the run; the
/// agent's note underneath.
struct PlanStepCheckRow: View {
    let row: PlanStepRow
    /// Off-plan attribution: how many activity rows the run recorded on this
    /// step; nil when the run is not attributed (no count shown).
    var activityCount: Int? = nil
    var expanded = false
    /// Opens or closes this step's activity; only offered with a count above 0.
    var onToggle: (() -> Void)? = nil
    @ObservedObject private var theme = ThemeManager.shared

    /// Mark column (18) + spacing (10): where the text — and the hairline — starts.
    static let textInset: CGFloat = 28

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            mark
                .frame(width: 18)
            VStack(alignment: .leading, spacing: 2) {
                HStack(alignment: .firstTextBaseline) {
                    Text(row.name)
                        .font(.themed(Lively.stepTitleSize, weight: row.mark == .inProgress ? .semibold : .regular))
                        .foregroundColor(dimmed ? theme.secondaryTextColor : theme.textColor)
                        .strikethrough(row.mark == .skipped, color: theme.secondaryTextColor)
                        .fixedSize(horizontal: false, vertical: true)
                    Spacer(minLength: 4)
                    if row.mark == .notReached {
                        Text("not reached")
                            .font(.themed(Lively.stepMetaSize, weight: .medium))
                            .foregroundColor(theme.secondaryTextColor)
                    }
                    if let activityCount {
                        countChip(activityCount)
                    }
                }
                if let note = row.note {
                    Text(note)
                        .font(.themed(Lively.stepMetaSize))
                        .foregroundColor(theme.secondaryTextColor)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
        .padding(.vertical, Lively.stepRowPadding)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(row.name), \(markWord)\(row.note.map { ". \($0)" } ?? "")\(activityWords)")
        .accessibilityAction(named: expanded ? "Hide activity" : "Show activity") {
            if (activityCount ?? 0) > 0 { onToggle?() }
        }
    }

    private var activityWords: String {
        guard let activityCount else { return "" }
        return ", \(activityCount) \(activityCount == 1 ? "activity" : "activities")"
    }

    private var dimmed: Bool { [.pending, .notReached, .skipped].contains(row.mark) }

    /// The step's activity count in a hairline rounded square; taps open the
    /// step's activity when there is any.
    @ViewBuilder
    private func countChip(_ count: Int) -> some View {
        let chip = Text("\(count)")
            .font(.themed(Lively.stepMetaSize, weight: expanded ? .semibold : .regular))
            .monospacedDigit()
            .foregroundColor(expanded ? theme.textColor : theme.secondaryTextColor)
            .padding(.horizontal, 5)
            .frame(minWidth: 20)
            .overlay(
                RoundedRectangle(cornerRadius: 4, style: .continuous)
                    .stroke(theme.secondaryTextColor.opacity(0.35), lineWidth: 0.5)
            )
        if count > 0, let onToggle {
            Button(action: onToggle) { chip }
                .buttonStyle(.plain)
                .accessibilityLabel("\(count) \(count == 1 ? "activity" : "activities") on this step")
                .accessibilityHint(expanded ? "Hides them" : "Shows them")
        } else {
            chip.opacity(0.6)
        }
    }

    private var markWord: String {
        switch row.mark {
        case .done: return "done"
        case .failed: return "failed"
        case .inProgress: return "in progress"
        case .pending: return "pending"
        case .skipped: return "skipped"
        case .notReached: return "not reached"
        }
    }

    @ViewBuilder
    private var mark: some View {
        switch row.mark {
        case .done:
            Image(systemName: "checkmark").font(.system(size: 11, weight: .bold)).foregroundColor(theme.successColor)
        case .failed:
            Image(systemName: "xmark").font(.system(size: 11, weight: .bold)).foregroundColor(theme.dangerColor)
        case .inProgress:
            StatusDot(tone: .running, size: 9)
        case .pending:
            Image(systemName: "circle").font(.system(size: 10)).foregroundColor(theme.secondaryTextColor)
        case .skipped:
            Image(systemName: "arrow.uturn.right").font(.system(size: 10, weight: .semibold))
                .foregroundColor(theme.secondaryTextColor)
        case .notReached:
            Image(systemName: "circle.dotted").font(.system(size: 10)).foregroundColor(theme.secondaryTextColor)
        }
    }
}

/// One run timeline event: a dot on a rail, the title, when, and the body.
struct TaskRunEventRow: View {
    let event: TaskDetailActivity
    let origin: Date?
    var delegatedAgent: String? = nil
    var isFirst = false
    var isLast = false
    @ObservedObject private var theme = ThemeManager.shared

    private var tone: StatusDot.Tone {
        switch event.status.lowercased() {
        case "running", "in_progress", "executing": return .running
        case "failed", "error": return .failed
        case "waiting", "paused", "warning": return .waiting
        case "done", "completed", "success", "succeeded": return .done
        default: return .notStarted
        }
    }

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            VStack(spacing: 0) {
                // The rail runs through the row's top padding so it never breaks.
                Rectangle().fill(isFirst ? Color.clear : theme.cardBorderColor)
                    .frame(width: 1, height: Lively.stepRowPadding + 4)
                StatusDot(tone: tone, size: 7)
                if !isLast {
                    Rectangle().fill(theme.cardBorderColor).frame(width: 1).frame(maxHeight: .infinity)
                }
            }
            .frame(width: 10)
            VStack(alignment: .leading, spacing: 2) {
                HStack(alignment: .firstTextBaseline) {
                    Text(delegatedAgent.map { "\(event.title) · \($0)" } ?? event.title)
                        .font(.themed(Lively.stepTitleSize, weight: .medium))
                        .foregroundColor(theme.textColor)
                        .fixedSize(horizontal: false, vertical: true)
                    Spacer(minLength: 4)
                    if let when = TaskTimelineFormatting.offset(event: event.timestamp, origin: origin)
                        ?? TaskTimelineFormatting.wallClock(event.timestamp) {
                        Text(when)
                            .font(.themedMono(Lively.stepMetaSize))
                            .foregroundColor(theme.secondaryTextColor)
                            .monospacedDigit()
                    }
                }
                if let body = event.body, !body.isEmpty {
                    Text(body)
                        .font(.themed(Lively.stepMetaSize))
                        .foregroundColor(theme.secondaryTextColor)
                        .lineLimit(3)
                        .fixedSize(horizontal: false, vertical: true)
                }
                if let model = event.model {
                    let tokens = [TaskTimelineFormatting.tokenCount(event.inputTokens),
                                  TaskTimelineFormatting.tokenCount(event.outputTokens)]
                        .compactMap { $0 }.joined(separator: " → ")
                    Text([model, tokens.isEmpty ? nil : "\(tokens) tok", TaskTimelineFormatting.usd(event.costUsd)]
                        .compactMap { $0 }.joined(separator: " · "))
                        .font(.themed(Lively.stepMetaSize))
                        .foregroundColor(theme.secondaryTextColor.opacity(0.85))
                        .lineLimit(1)
                }
            }
            .padding(.vertical, Lively.stepRowPadding)
            .overlay(alignment: .top) { if !isFirst { StepHairline() } }
        }
    }
}

// MARK: - Plan

struct TaskDetailPlanBody: View {
    let snapshot: TaskDetailSnapshot
    @ObservedObject var model: TaskDetailScreenModel
    let onNotice: (String) -> Void
    @ObservedObject private var theme = ThemeManager.shared
    @State private var approving = false
    @Environment(\.attentionFeed) private var attentionFeed

    /// The screen's ask card (`TaskDetailAskSection`) answers these questions
    /// in place when it holds them; only an ask it cannot see hands off.
    private var questionsAnsweredInPlace: Bool {
        let ids = Set(snapshot.questions.compactMap(\.attentionTargetID))
        guard !ids.isEmpty else { return false }
        let feed = (attentionFeed?.items("all") ?? []).filter { $0.owningTaskId == model.seed.taskId }
        return (snapshot.askRows + feed).contains { ids.contains($0.correlationId) || ids.contains($0.id) }
    }

    var body: some View {
        let steps = TaskDetailLayout.planSteps(snapshot)
        VStack(alignment: .leading, spacing: 12) {
            if !snapshot.questions.isEmpty {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(snapshot.questions) { question in
                        Text(question.text)
                            .font(.themed(14, weight: .semibold))
                            .foregroundColor(theme.textColor)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    if questionsAnsweredInPlace {
                        Text("Answer it in the Waiting on you card above.")
                            .font(.themed(12))
                            .foregroundColor(theme.secondaryTextColor)
                    } else {
                        Button { AppActions.shared.requestAttention(itemID: snapshot.questions.lazy.compactMap(\.attentionTargetID).first ?? snapshot.attentionTargetID) } label: {
                            Label("Answer in Attention", systemImage: "bell.badge")
                        }
                        .buttonStyle(ChipStyle(prominent: true))
                    }
                }
            }
            if steps.isEmpty {
                Text(TaskDetailLayout.planningActivityIsLive(snapshot)
                     ? "The steps appear once the plan is drafted."
                     : "This plan has no steps to show.")
                    .font(.themed(14))
                    .foregroundColor(theme.secondaryTextColor)
            } else {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(steps.enumerated()), id: \.offset) { index, step in
                        if index > 0 { StepHairline(leadingInset: 28) }
                        HStack(alignment: .firstTextBaseline, spacing: 10) {
                            Text("\(index + 1)")
                                .font(.themedMono(Lively.stepMetaSize, weight: .semibold))
                                .foregroundColor(theme.secondaryTextColor)
                                .frame(width: 18, alignment: .trailing)
                            Text(step)
                                .font(.themed(Lively.stepTitleSize))
                                .foregroundColor(theme.textColor)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                        .padding(.vertical, Lively.stepRowPadding)
                    }
                }
                .accessibilityIdentifier("task-detail-plan-list")
            }
            if snapshot.planStatus == "draft" { draftActions }
            if !snapshot.planningEntries.isEmpty {
                PlanningActivity(entries: snapshot.planningEntries,
                                 live: TaskDetailLayout.planningActivityIsLive(snapshot))
            }
        }
    }

    /// A draft plan: Reject · Approve · Approve & Run, the same order and
    /// two-step rule as an approval prompt (`PlanApprovalActions`).
    private var draftActions: some View {
        HStack(spacing: 6) {
            Button(PlanApprovalChoice.reject.label()) { Task { await model.perform(.rejectPlan) } }
                .buttonStyle(ChipStyle())
            Button(PlanApprovalChoice.approve.label()) {
                Task { if await model.perform(.approvePlan) { onNotice(PlanApprovalOutcome.approved.message) } }
            }
            .buttonStyle(ChipStyle())
            Button(PlanApprovalChoice.approveAndRun.label()) {
                approving = true
                Task {
                    let outcome = await PlanApprovalActions.approveAndRun(
                        taskId: model.seed.taskId,
                        approve: { await model.perform(.approvePlan) ? .ok : .failed("") },
                        startRun: { _ in
                            guard await model.perform(.run) else { throw URLError(.cannotLoadFromNetwork) }
                        })
                    approving = false
                    if outcome.isApproved { onNotice(outcome == .approvedAndStarted ? outcome.message : "Approved — the run did not start.") }
                }
            }
            .buttonStyle(ChipStyle(prominent: true))
            .disabled(approving)
        }
    }
}

/// Planning activity: a live list of milestones while planning (model calls
/// as compact secondary rows, warnings bold), then folded under
/// `Planning activity · N events · duration`.
struct PlanningActivity: View {
    let entries: [PlanningEntry]
    let live: Bool
    @ObservedObject private var theme = ThemeManager.shared
    @State private var expanded = false

    var body: some View {
        if live {
            VStack(alignment: .leading, spacing: 6) {
                Text("Planning activity\(TaskUsageFormat.planningSuffix(entries))")
                    .font(.themed(12, weight: .semibold))
                    .foregroundColor(theme.secondaryTextColor)
                list
            }
        } else {
            DisclosureGroup(isExpanded: $expanded) {
                list.padding(.top, 6)
            } label: {
                Text("Planning activity · \(PlanProgress.activitySummary(entries))\(TaskUsageFormat.planningSuffix(entries))")
                    .font(.themed(13, weight: .semibold))
                    .foregroundColor(theme.textColor)
                    .frame(minHeight: Lively.tapMin)
            }
            .tint(theme.secondaryTextColor)
            .accessibilityIdentifier("task-detail-planning-activity")
        }
    }

    private var list: some View {
        TimelineView(.periodic(from: .now, by: 15)) { context in
            VStack(alignment: .leading, spacing: 0) {
                ForEach(Array(entries.enumerated()), id: \.element.id) { index, entry in
                    if index > 0 { StepHairline(leadingInset: 18) }
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        if entry.isModelCall {
                            Image(systemName: "cpu").font(.system(size: 9)).foregroundColor(theme.secondaryTextColor)
                                .frame(width: 10)
                        } else {
                            StatusDot(tone: entry.warning ? .waiting : .done, size: 6).frame(width: 10)
                        }
                        Text(line(entry))
                            .font(.themed(entry.isModelCall ? Lively.stepMetaSize : Lively.stepTitleSize,
                                          weight: entry.warning ? .bold : .regular))
                            .foregroundColor(entry.isModelCall ? theme.secondaryTextColor : theme.textColor)
                            .fixedSize(horizontal: false, vertical: true)
                        Spacer(minLength: 4)
                        Text(ActSummaries.compactRelativeTime(entry.at, now: context.date))
                            .font(.themedMono(Lively.stepMetaSize))
                            .foregroundColor(theme.secondaryTextColor)
                            .monospacedDigit()
                    }
                    .padding(.vertical, Lively.stepRowPadding)
                }
            }
        }
    }

    private func line(_ entry: PlanningEntry) -> String {
        guard entry.isModelCall, let cost = TaskTimelineFormatting.usd(entry.costUsd) else { return entry.message }
        return "\(entry.message) · \(cost)"
    }
}

// MARK: - Technical details

/// Collapsed, last: the task's other files, the run picker and history, ids.
struct TaskDetailTechnicalCard: View {
    let snapshot: TaskDetailSnapshot
    let taskId: String
    @ObservedObject var model: TaskDetailScreenModel
    let onNotice: (String) -> Void
    @ObservedObject private var theme = ThemeManager.shared
    @State private var expanded = false

    var body: some View {
        DisclosureGroup(isExpanded: $expanded) {
            VStack(alignment: .leading, spacing: 16) {
                otherFiles
                if let schedule = model.recurringSchedule {
                    VStack(alignment: .leading, spacing: 6) {
                        Text("App schedule").font(.themed(13, weight: .bold)).foregroundColor(theme.textColor)
                        ForEach(["behavior_id", "interval_seconds", "next_due_at", "latest_status", "waiting_for_settlement"], id: \.self) { key in
                            if let value = schedule[key], !(value is NSNull) { Text(key.replacingOccurrences(of: "_", with: " ").capitalized + ": " + String(describing: value)).font(.themed(12)).foregroundColor(theme.secondaryTextColor) }
                        }
                    }
                }
                runs
                if model.historyLoading { ProgressView("Loading older runs") }
                if let error = model.historyError { Text(error).foregroundColor(theme.dangerColor) }
                if model.historyHasMore { Button(model.historyError == nil ? "Load older runs" : "Retry older runs") { Task { await model.loadOlderExecutions() } }.disabled(model.historyLoading).frame(minHeight: 44) }
                ids
                WorkRelatedTools(origin: WorkWebContext(section: snapshot.isExecutionOnly ? .crew : .all, agent: snapshot.agentId ?? "", task: snapshot.isExecutionOnly ? "" : taskId, execution: model.selectedExecutionId ?? "", internalWork: ["internal", "ephemeral_owned_by_chat", "internal_debug"].contains(snapshot.lifecycle ?? ""), appInstallation: model.appInstallationID ?? ""), destinations: [.analytics, .history, .memory, .events, .evals, .budget] + (TaskWorkOwnership.isAppManaged(taskId) ? [.apps] : []))
            }
            .padding(.top, 8)
        } label: {
            Text("Technical details")
                .font(.themed(15, weight: .semibold))
                .foregroundColor(theme.textColor)
                .frame(minHeight: Lively.tapMin)
        }
        .tint(theme.secondaryTextColor)
        .padding(.horizontal, 14)
        .padding(.vertical, 4)
        .background(theme.cardColor.opacity(0.6))
        .clipShape(RoundedRectangle(cornerRadius: Lively.cardRadius, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: Lively.cardRadius, style: .continuous)
                .strokeBorder(theme.cardBorderColor, lineWidth: 1))
        .accessibilityIdentifier("task-detail-technical")
    }

    @ViewBuilder
    private var otherFiles: some View {
        let sections = TaskDetailLayout.technicalSections(snapshot)
        if !sections.isEmpty {
            VStack(alignment: .leading, spacing: 10) {
                ForEach(sections) { section in
                    VStack(alignment: .leading, spacing: 6) {
                        Text(section.scope.title)
                            .font(.themed(13, weight: .bold))
                            .foregroundColor(theme.textColor)
                        ForEach(section.files) { file in
                            TaskOutputFileRow(artifact: file, taskId: taskId, onNotice: onNotice)
                            if let detail = file.detail {
                                Text(detail).font(.themed(11)).foregroundColor(theme.secondaryTextColor)
                            }
                        }
                        ForEach(section.structured) { item in
                            Text([item.name, item.artifactType].compactMap { $0 }.joined(separator: " · "))
                                .font(.themed(12))
                                .foregroundColor(theme.secondaryTextColor)
                        }
                    }
                    .accessibilityIdentifier("task-detail-output-scope-\(section.scope.rawValue)")
                }
            }
        }
    }

    @ViewBuilder
    private var runs: some View {
        if !snapshot.history.isEmpty {
            VStack(alignment: .leading, spacing: 8) {
                Text("Runs").font(.themed(13, weight: .bold)).foregroundColor(theme.textColor)
                ForEach(snapshot.history) { run in
                    Button { model.selectExecution(run.id) } label: {
                        HStack(spacing: 8) {
                            Image(systemName: run.id == model.selectedExecutionId ? "checkmark.circle.fill" : "circle")
                                .foregroundColor(run.id == model.selectedExecutionId ? theme.accentColor : theme.secondaryTextColor)
                            VStack(alignment: .leading, spacing: 1) {
                                Text(TaskDetailScreen.shortId(run.id))
                                    .font(.themedMono(12))
                                    .foregroundColor(theme.textColor)
                                Text([run.status.replacingOccurrences(of: "_", with: " "),
                                      TaskHistoryFormatting.runSpan(startedAt: run.startedAt, endedAt: run.endedAt,
                                                                    clock: Self.clock)]
                                    .compactMap { $0 }.joined(separator: " · "))
                                    .font(.themed(11))
                                    .foregroundColor(theme.secondaryTextColor)
                            }
                            Spacer()
                        }
                        .frame(minHeight: Lively.tapMin)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Show run \(run.id)")
                }
            }
            .accessibilityIdentifier("task-detail-runs")
        }
    }

    private var ids: some View {
        VStack(alignment: .leading, spacing: 6) {
            idLine("Task", taskId)
            if let execution = model.selectedExecutionId { idLine("Run", execution) }
            if let plan = snapshot.planId { idLine("Plan", plan) }
            if let agent = snapshot.agentId { idLine("Agent", agent) }
            if let thread = snapshot.threadId { idLine("Thread", thread) }
            if let chat = snapshot.chatSessionId { idLine("Chat session", chat) }
            if let lifecycle = snapshot.lifecycle { idLine("Lifecycle", lifecycle) }
        }
    }

    private func idLine(_ label: String, _ value: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(label).font(.themed(12)).foregroundColor(theme.secondaryTextColor)
                .frame(width: 84, alignment: .leading)
            Text(value).font(.themedMono(11)).foregroundColor(theme.textColor)
                .textSelection(.enabled)
                .lineLimit(2)
        }
    }

    private static func clock(_ date: Date) -> String {
        let formatter = DateFormatter()
        formatter.timeStyle = .short
        return formatter.string(from: date)
    }
}

// MARK: - Notice

struct TaskDetailNotice: View {
    let text: String
    let tint: Color
    let icon: String
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: icon).foregroundColor(tint)
            Text(text).font(.themed(13)).foregroundColor(theme.textColor)
                .frame(maxWidth: .infinity, alignment: .leading)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(10)
        .background(tint.opacity(0.1))
        .clipShape(RoundedRectangle(cornerRadius: 10))
    }
}
