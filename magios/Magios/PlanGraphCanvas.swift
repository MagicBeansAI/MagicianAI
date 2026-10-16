import SwiftUI

/// The plan as a layered top-to-bottom DAG (`PlanGraphLayout`) in a pinch-
/// to-zoom, drag-to-pan viewport (0.3–3×, Fit). Nodes are compact cards:
/// confidence fill, dashed border when the tool is unresolved, ▶ on a start
/// step, "via agent" when delegated, and the live run mark during a run.
/// Tapping a node opens its step sheet.
struct PlanGraphCanvas: View {
    let graph: PlanGraph
    var marks: [String: PlanStepMark] = [:]
    var onSelect: (String) -> Void

    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    @State private var scale: CGFloat = 1
    @State private var offset: CGSize = .zero
    @State private var viewport: CGSize = .zero
    @State private var fittedFor: String = ""
    @GestureState private var pinch: CGFloat = 1
    @GestureState private var pan: CGSize = .zero

    private var layout: PlanGraphLayout { PlanGraphLayout(graph: graph) }

    var body: some View {
        let layout = self.layout
        GeometryReader { proxy in
            let shownScale = PlanGraphLayout.clampScale(scale * pinch)
            let shownOffset = Self.offset(offset, zoomingFrom: scale, to: shownScale, around: proxy.size) + pan
            ZStack(alignment: .topLeading) {
                theme.backgroundColor.opacity(0.001)
                graphContent(layout)
                    .frame(width: layout.size.width, height: layout.size.height, alignment: .topLeading)
                    .scaleEffect(shownScale, anchor: .topLeading)
                    .offset(shownOffset)
            }
            .frame(width: proxy.size.width, height: proxy.size.height, alignment: .topLeading)
            .clipped()
            .contentShape(Rectangle())
            .gesture(panGesture.simultaneously(with: zoomGesture(viewport: proxy.size)))
            .onAppear { viewport = proxy.size; fitIfNeeded(layout) }
            .onChange(of: proxy.size) { _, size in viewport = size; fitIfNeeded(layout, force: true) }
            .onChange(of: graph.steps.map(\.id)) { _, _ in fitIfNeeded(layout, force: true) }
        }
        .overlay(alignment: .bottomTrailing) { controls(layout) }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Plan graph")
    }

    // MARK: Content

    private func graphContent(_ layout: PlanGraphLayout) -> some View {
        let order = Dictionary(graph.waterfallOrder.enumerated().map { ($0.element, $0.offset + 1) },
                               uniquingKeysWith: { first, _ in first })
        return ZStack(alignment: .topLeading) {
            Canvas { context, _ in
                let color = theme.secondaryTextColor.opacity(0.55)
                for segment in layout.segments(for: graph) {
                    var path = Path()
                    path.move(to: segment.from)
                    let bend = max((segment.to.y - segment.from.y) * 0.45, 12)
                    path.addCurve(to: segment.to,
                                  control1: CGPoint(x: segment.from.x, y: segment.from.y + bend),
                                  control2: CGPoint(x: segment.to.x, y: segment.to.y - bend))
                    context.stroke(path, with: .color(color), lineWidth: 1.5)
                    context.fill(Self.arrowhead(at: segment.to, from: CGPoint(x: segment.to.x, y: segment.to.y - bend)),
                                 with: .color(color))
                }
            }
            .frame(width: layout.size.width, height: layout.size.height)
            .accessibilityHidden(true)

            ForEach(graph.steps) { step in
                if let center = layout.positions[step.id] {
                    PlanGraphNode(step: step, number: order[step.id] ?? 0, mark: marks[step.id],
                                  size: layout.metrics.nodeSize) { onSelect(step.id) }
                        .position(center)
                }
            }
        }
    }

    static func arrowhead(at tip: CGPoint, from control: CGPoint, length: CGFloat = 9, width: CGFloat = 7) -> Path {
        let angle = atan2(tip.y - control.y, tip.x - control.x)
        let back = CGPoint(x: tip.x - length * cos(angle), y: tip.y - length * sin(angle))
        let normal = CGPoint(x: -sin(angle) * width / 2, y: cos(angle) * width / 2)
        var path = Path()
        path.move(to: tip)
        path.addLine(to: CGPoint(x: back.x + normal.x, y: back.y + normal.y))
        path.addLine(to: CGPoint(x: back.x - normal.x, y: back.y - normal.y))
        path.closeSubpath()
        return path
    }

    // MARK: Zoom and pan

    private var panGesture: some Gesture {
        DragGesture(minimumDistance: 4)
            .updating($pan) { value, state, _ in state = value.translation }
            .onEnded { value in
                offset = CGSize(width: offset.width + value.translation.width,
                                height: offset.height + value.translation.height)
            }
    }

    private func zoomGesture(viewport: CGSize) -> some Gesture {
        MagnifyGesture()
            .updating($pinch) { value, state, _ in state = value.magnification }
            .onEnded { value in
                let next = PlanGraphLayout.clampScale(scale * value.magnification)
                offset = Self.offset(offset, zoomingFrom: scale, to: next, around: viewport)
                scale = next
            }
    }

    /// Keep the viewport's centre fixed while the scale changes.
    static func offset(_ offset: CGSize, zoomingFrom old: CGFloat, to new: CGFloat, around viewport: CGSize) -> CGSize {
        guard old > 0 else { return offset }
        let center = CGPoint(x: viewport.width / 2, y: viewport.height / 2)
        let ratio = new / old
        return CGSize(width: center.x - (center.x - offset.width) * ratio,
                      height: center.y - (center.y - offset.height) * ratio)
    }

    private func zoom(by factor: CGFloat) {
        let next = PlanGraphLayout.clampScale(scale * factor)
        let apply = {
            offset = Self.offset(offset, zoomingFrom: scale, to: next, around: viewport)
            scale = next
        }
        if reduceMotion { apply() } else { withAnimation(.easeInOut(duration: 0.2), apply) }
    }

    private func fit(_ layout: PlanGraphLayout) {
        let next = layout.fitScale(in: viewport)
        let apply = {
            scale = next
            offset = CGSize(width: (viewport.width - layout.size.width * next) / 2,
                            height: max((viewport.height - layout.size.height * next) / 2, 0))
        }
        if reduceMotion { apply() } else { withAnimation(.easeInOut(duration: 0.25), apply) }
    }

    private func fitIfNeeded(_ layout: PlanGraphLayout, force: Bool = false) {
        let key = "\(graph.steps.count)|\(Int(viewport.width))x\(Int(viewport.height))"
        guard viewport.width > 0, force || fittedFor != key else { return }
        fittedFor = key
        fit(layout)
    }

    private func controls(_ layout: PlanGraphLayout) -> some View {
        HStack(spacing: 0) {
            controlButton("minus.magnifyingglass", "Zoom out") { zoom(by: 0.75) }
            controlButton("plus.magnifyingglass", "Zoom in") { zoom(by: 1.3) }
            controlButton("arrow.down.right.and.arrow.up.left", "Fit to screen") { fit(layout) }
        }
        .background(theme.cardColor.opacity(0.95))
        .clipShape(RoundedRectangle(cornerRadius: Lively.chipRadius + 4, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Lively.chipRadius + 4, style: .continuous)
            .strokeBorder(theme.cardBorderColor, lineWidth: 1))
        .padding(12)
    }

    private func controlButton(_ icon: String, _ label: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: icon)
                .font(.system(size: 15, weight: .semibold))
                .foregroundColor(theme.textColor)
                .frame(width: Lively.tapMin, height: Lively.tapMin)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(label)
        .accessibilityIdentifier("plan-graph-\(label.lowercased().replacingOccurrences(of: " ", with: "-"))")
    }
}

private extension CGSize {
    static func + (lhs: CGSize, rhs: CGSize) -> CGSize { CGSize(width: lhs.width + rhs.width, height: lhs.height + rhs.height) }
}

/// One node card on the graph.
struct PlanGraphNode: View {
    let step: PlanStep
    let number: Int
    let mark: PlanStepMark?
    let size: CGSize
    let onTap: () -> Void

    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        let band = PlanConfidenceBand(step.confidence)
        let bandColor = PlanStyle.color(for: band, theme: theme)
        let running = mark == .inProgress
        Button(action: onTap) {
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 4) {
                    if step.isStart {
                        Text("▶\u{FE0E}").font(.system(size: 9, weight: .bold))
                            .foregroundColor(theme.onAccentColor)
                            .frame(width: 16, height: 16)
                            .background(RoundedRectangle(cornerRadius: 4, style: .continuous).fill(theme.accentFillColor))
                    }
                    Text("\(number)").font(.themedMono(10, weight: .semibold)).foregroundColor(theme.secondaryTextColor)
                    Spacer(minLength: 2)
                    Text("\(Int((step.confidence * 100).rounded()))%")
                        .font(.themedMono(10, weight: .semibold)).foregroundColor(bandColor)
                    if let mark { StatusDot(tone: PlanStyle.tone(for: mark), size: 8) }
                }
                Text(step.task.isEmpty ? step.id : step.task)
                    .font(.themed(11, weight: .semibold))
                    .foregroundColor(theme.textColor)
                    .lineLimit(2)
                    .multilineTextAlignment(.leading)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Spacer(minLength: 0)
                if let agent = step.providingAgentId, !agent.isEmpty {
                    Text("via \(agent)")
                        .font(.themed(9, weight: .semibold))
                        .lineLimit(1)
                        .foregroundColor(theme.secondaryAccentColor)
                        .padding(.horizontal, 5).padding(.vertical, 1)
                        .background(RoundedRectangle(cornerRadius: 4, style: .continuous)
                            .fill(theme.secondaryAccentColor.opacity(0.14)))
                } else {
                    Text(step.hasResolvedTool ? (step.tool ?? "") : "No tool")
                        .font(.themedMono(9))
                        .lineLimit(1)
                        .foregroundColor(step.hasResolvedTool ? theme.secondaryTextColor : theme.warningColor)
                }
            }
            .padding(8)
            .frame(width: size.width, height: size.height, alignment: .topLeading)
            .background(
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .fill(theme.cardColor)
                    .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous).fill(bandColor.opacity(0.14)))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .strokeBorder(
                        running ? theme.statusRunningColor
                            : step.isDelegated ? theme.secondaryAccentColor : bandColor.opacity(0.75),
                        style: StrokeStyle(lineWidth: running ? 2.5 : 1.5,
                                           dash: step.hasResolvedTool || step.isDelegated ? [] : [5, 4]))
            )
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(PlanStyle.accessibilityLabel(step: step, number: number, mark: mark))
        .accessibilityHint("Opens the step")
        .accessibilityIdentifier("plan-graph-node-\(step.id)")
    }
}
