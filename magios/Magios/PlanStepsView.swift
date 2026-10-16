import SwiftUI

/// Colours and words shared by the inspector's views.
enum PlanStyle {
    static func color(for band: PlanConfidenceBand, theme: ThemeManager) -> Color {
        switch band {
        case .high: return theme.successColor
        case .medium: return theme.warningColor
        case .low: return theme.dangerColor
        }
    }

    static func tone(for mark: PlanStepMark) -> StatusDot.Tone {
        switch mark {
        case .done: return .done
        case .failed: return .failed
        case .inProgress: return .running
        case .pending: return .notStarted
        case .skipped, .notReached: return .cancelled
        }
    }

    static func markLabel(_ mark: PlanStepMark) -> String {
        switch mark {
        case .done: return "done"
        case .failed: return "failed"
        case .inProgress: return "running"
        case .pending: return "not started"
        case .skipped: return "skipped"
        case .notReached: return "not reached"
        }
    }

    static func percent(_ value: Double?) -> String {
        guard let value, value.isFinite else { return "—" }
        return "\(Int((value * 100).rounded()))%"
    }

    static func accessibilityLabel(step: PlanStep, number: Int, mark: PlanStepMark?) -> String {
        var parts = ["Step \(number)", step.task.isEmpty ? step.id : step.task, "confidence \(percent(step.confidence))"]
        if let agent = step.providingAgentId, !agent.isEmpty { parts.append("delegated to \(agent)") }
        else { parts.append(step.hasResolvedTool ? "tool \(step.tool ?? "")" : "no tool resolved") }
        if step.isStart { parts.append("start step") }
        if let mark { parts.append(markLabel(mark)) }
        return parts.joined(separator: ", ")
    }

    static func truncate(_ text: String, _ length: Int) -> String {
        text.count > length ? String(text.prefix(length)) + "…" : text
    }
}

/// A small non-interactive tag: rounded square, themed.
struct PlanTag: View {
    let text: String
    var color: Color? = nil
    var mono = false

    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        Text(text)
            .font(mono ? .themedMono(11, weight: .semibold) : .themed(11, weight: .semibold))
            .lineLimit(1)
            .foregroundColor(color ?? theme.secondaryTextColor)
            .padding(.horizontal, 7)
            .padding(.vertical, 3)
            .background(RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous)
                .fill((color ?? theme.secondaryTextColor).opacity(0.12)))
    }
}

/// Collapsible pretty-printed JSON.
struct PlanJSONDisclosure: View {
    let title: String
    let value: PlanJSON
    var expanded = false

    @ObservedObject private var theme = ThemeManager.shared
    @State private var open: Bool?

    var body: some View {
        DisclosureGroup(isExpanded: Binding(get: { open ?? expanded }, set: { open = $0 })) {
            ScrollView(.horizontal) {
                Text(value.prettyJSON)
                    .font(.themedMono(11))
                    .foregroundColor(theme.textColor)
                    .textSelection(.enabled)
                    .padding(8)
            }
            .frame(maxHeight: 360)
            .background(RoundedRectangle(cornerRadius: 8, style: .continuous).fill(theme.surfaceColor))
        } label: {
            Text(title).font(.themed(13, weight: .semibold)).foregroundColor(theme.textColor)
                .frame(minHeight: Lively.tapMin, alignment: .leading)
        }
        .tint(theme.accentColor)
    }
}

/// A card container for the inspector's lists.
struct PlanCard<Content: View>: View {
    @ViewBuilder var content: Content
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 8) { content }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(theme.cardColor)
            .clipShape(RoundedRectangle(cornerRadius: Lively.cardRadius, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: Lively.cardRadius, style: .continuous)
                .strokeBorder(theme.cardBorderColor, lineWidth: 1))
    }
}

// MARK: - Steps

/// The waterfall: steps in topological order with their levels, tool status,
/// dependencies (with reasons) and up to three parameters. In edit mode it
/// becomes a list: drag to reorder, swipe to delete, Add step.
struct PlanStepsView: View {
    let graph: PlanGraph
    var marks: [String: PlanStepMark] = [:]
    var editing = false
    var onSelect: (String) -> Void
    var onMove: (IndexSet, Int) -> Void = { _, _ in }
    var onDelete: (IndexSet) -> Void = { _ in }
    var onAdd: () -> Void = {}

    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        if editing { editList } else { waterfall }
    }

    private var waterfall: some View {
        ScrollView {
            LazyVStack(spacing: 6) {
                ForEach(Array(graph.waterfall.enumerated()), id: \.element.step.id) { index, item in
                    Button { onSelect(item.step.id) } label: {
                        WaterfallCard(graph: graph, step: item.step, number: index + 1, level: item.level,
                                      mark: marks[item.step.id])
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("plan-step-\(item.step.id)")
                }
            }
            .padding(16)
        }
    }

    private var editList: some View {
        List {
            Section {
                ForEach(Array(graph.steps.enumerated()), id: \.element.id) { index, step in
                    Button { onSelect(step.id) } label: { editRow(step, number: index + 1) }
                        .buttonStyle(.plain)
                        .listRowBackground(theme.cardColor)
                        .accessibilityIdentifier("plan-edit-step-\(step.id)")
                }
                .onMove(perform: onMove)
                .onDelete(perform: onDelete)
            } footer: {
                Text("Hold and drag to reorder. Swipe left to delete. Tap a step to edit it and its dependencies.")
                    .font(.themed(12)).foregroundColor(theme.secondaryTextColor)
            }
            Section {
                Button(action: onAdd) {
                    Label("Add step", systemImage: "plus.circle.fill")
                        .font(.themed(15, weight: .semibold))
                        .foregroundColor(theme.accentColor)
                        .frame(maxWidth: .infinity, minHeight: Lively.tapMin, alignment: .leading)
                }
                .listRowBackground(theme.cardColor)
                .accessibilityIdentifier("plan-add-step")
            }
        }
        .scrollContentBackground(.hidden)
    }

    private func editRow(_ step: PlanStep, number: Int) -> some View {
        HStack(alignment: .top, spacing: 10) {
            Text("\(number)").font(.themedMono(12, weight: .semibold)).foregroundColor(theme.secondaryTextColor)
                .frame(minWidth: 18)
            VStack(alignment: .leading, spacing: 4) {
                Text(step.task.isEmpty ? step.id : step.task)
                    .font(.themed(Lively.stepTitleSize, weight: .semibold)).foregroundColor(theme.textColor)
                    .frame(maxWidth: .infinity, alignment: .leading)
                HStack(spacing: 6) {
                    PlanTag(text: PlanStyle.percent(step.confidence),
                            color: PlanStyle.color(for: PlanConfidenceBand(step.confidence), theme: theme), mono: true)
                    if step.isStart { PlanTag(text: "▶\u{FE0E} start", color: theme.accentColor) }
                    else { PlanTag(text: "after \(step.dependsOn.count)") }
                    PlanTag(text: step.hasResolvedTool ? (step.tool ?? "") : "no tool",
                            color: step.hasResolvedTool ? nil : theme.warningColor, mono: step.hasResolvedTool)
                }
            }
            Image(systemName: "chevron.right").font(.system(size: 12, weight: .semibold))
                .foregroundColor(theme.secondaryTextColor)
        }
        .frame(minHeight: Lively.tapMin)
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
    }
}

/// One read-only waterfall card (web `.waterfall-step`).
struct WaterfallCard: View {
    let graph: PlanGraph
    let step: PlanStep
    let number: Int
    let level: Int
    let mark: PlanStepMark?

    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        HStack(spacing: 0) {
            if level > 0 {
                Rectangle().fill(theme.accentColor.opacity(0.6)).frame(width: CGFloat(1 + min(level, 4)))
            }
            VStack(alignment: .leading, spacing: 5) {
                header
                toolRow
                if !step.dependsOn.isEmpty { dependencies }
                if !step.parameters.isEmpty { parameters }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(theme.cardColor)
        .clipShape(RoundedRectangle(cornerRadius: Lively.cardRadius, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Lively.cardRadius, style: .continuous)
            .strokeBorder(mark == .inProgress ? theme.statusRunningColor
                          : step.isDelegated ? theme.secondaryAccentColor.opacity(0.6) : theme.cardBorderColor,
                          lineWidth: mark == .inProgress ? 2 : 1))
        .padding(.leading, CGFloat(min(level, 4)) * 10)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(PlanStyle.accessibilityLabel(step: step, number: number, mark: mark))
    }

    private var header: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text("\(number)")
                .font(.themedMono(11, weight: .bold)).foregroundColor(theme.onAccentColor)
                .frame(minWidth: 20, minHeight: 20)
                .background(RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous).fill(theme.accentFillColor))
            if level > 0 { PlanTag(text: "L\(level)", mono: true) }
            Text(step.task.isEmpty ? step.id : step.task)
                .font(.themed(Lively.stepTitleSize, weight: .semibold)).foregroundColor(theme.textColor)
                .frame(maxWidth: .infinity, alignment: .leading)
            VStack(alignment: .trailing, spacing: 4) {
                PlanTag(text: PlanStyle.percent(step.confidence),
                        color: PlanStyle.color(for: PlanConfidenceBand(step.confidence), theme: theme), mono: true)
                if let mark {
                    HStack(spacing: 4) {
                        StatusDot(tone: PlanStyle.tone(for: mark), size: 7)
                        Text(PlanStyle.markLabel(mark)).font(.themed(Lively.stepMetaSize)).foregroundColor(theme.secondaryTextColor)
                    }
                }
            }
        }
    }

    private var toolRow: some View {
        HStack(spacing: 6) {
            Text(step.isDelegated ? "Delegate" : "Tool").font(.themed(Lively.stepMetaSize, weight: .semibold))
                .foregroundColor(theme.secondaryTextColor)
            if let agent = step.providingAgentId, step.isDelegated {
                PlanTag(text: "via \(agent)", color: theme.secondaryAccentColor)
            } else if step.hasResolvedTool {
                Text(step.tool ?? "").font(.themedMono(Lively.stepMetaSize)).foregroundColor(theme.textColor).lineLimit(1)
                Text("✓").font(.system(size: 10, weight: .bold)).foregroundColor(theme.successColor)
                    .accessibilityLabel("resolved")
            } else {
                Text("⚠ Not resolved").font(.themed(Lively.stepMetaSize)).foregroundColor(theme.warningColor)
            }
        }
    }

    private var dependencies: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text("Depends on").font(.themed(Lively.stepMetaSize, weight: .semibold)).foregroundColor(theme.secondaryTextColor)
            ForEach(step.dependsOn, id: \.self) { dep in
                HStack(alignment: .firstTextBaseline, spacing: 4) {
                    Image(systemName: "arrow.turn.down.right").font(.system(size: 10))
                        .foregroundColor(theme.secondaryTextColor)
                    Text(PlanStyle.truncate(graph.step(dep).map { $0.task.isEmpty ? $0.id : $0.task } ?? dep, 40))
                        .font(.themed(Lively.stepMetaSize)).foregroundColor(theme.textColor)
                    if let reason = graph.reason(from: dep, to: step.id), !reason.isEmpty {
                        Text("(\(reason))").font(.themed(Lively.stepMetaSize)).foregroundColor(theme.secondaryTextColor).lineLimit(1)
                    }
                }
            }
        }
    }

    private var parameters: some View {
        let shown = step.parameters.prefix(3)
        return VStack(alignment: .leading, spacing: 4) {
            Text("Parameters").font(.themed(Lively.stepMetaSize, weight: .semibold)).foregroundColor(theme.secondaryTextColor)
            FlowRow(spacing: 6) {
                ForEach(Array(shown)) { parameter in
                    PlanTag(text: "\(parameter.key): \(PlanStyle.truncate(parameter.value.displayText, 20))", mono: true)
                }
                if step.parameters.count > 3 { PlanTag(text: "+\(step.parameters.count - 3) more") }
            }
        }
    }
}

/// Lays children left to right, wrapping onto new lines.
struct FlowRow: Layout {
    var spacing: CGFloat = 6

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let width = proposal.width ?? .infinity
        var x: CGFloat = 0, y: CGFloat = 0, line: CGFloat = 0, widest: CGFloat = 0
        for subview in subviews {
            let size = subview.sizeThatFits(.unspecified)
            if x > 0, x + size.width > width { x = 0; y += line + spacing; line = 0 }
            x += size.width + spacing
            line = max(line, size.height)
            widest = max(widest, x - spacing)
        }
        return CGSize(width: min(widest, width), height: y + line)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var x = bounds.minX, y = bounds.minY, line: CGFloat = 0
        for subview in subviews {
            let size = subview.sizeThatFits(.unspecified)
            if x > bounds.minX, x + size.width > bounds.maxX { x = bounds.minX; y += line + spacing; line = 0 }
            subview.place(at: CGPoint(x: x, y: y), proposal: ProposedViewSize(size))
            x += size.width + spacing
            line = max(line, size.height)
        }
    }
}
