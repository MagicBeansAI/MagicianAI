import ActivityKit
import SwiftUI
import WidgetKit

/// Lock screen + Dynamic Island for "Magician at work": the counts, the
/// featured item (kind icon, title, project, step), the next two items and
/// "+N more". The island shows the featured item's state dot, the running
/// count (or "!" when something needs the owner), and expanded, one button:
/// Approve for a run's diff, else Answer / Open into the app.
///
/// A content state from an older server has no featured item; the activity
/// then shows its headline and detail as before.
struct CodingRunsLiveActivity: Widget {
    typealias Summary = CodingRunsActivityAttributes.ContentState

    var body: some WidgetConfiguration {
        ActivityConfiguration(for: CodingRunsActivityAttributes.self) { context in
            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 10) {
                    Text("Magician at work")
                        .font(.subheadline.weight(.semibold))
                        .foregroundColor(.white)
                    Spacer()
                    Text(WorkActivityStyle.header(context.state))
                        .font(.caption.monospacedDigit())
                        .foregroundColor(context.state.needsYou > 0 ? .orange : .white.opacity(0.7))
                        .lineLimit(1)
                }
                if context.state.hasFeatured {
                    HStack(alignment: .center, spacing: 10) {
                        featuredRow(context.state)
                        actionButton(context.state, compact: false)
                    }
                    nextRows(context.state, limit: 2)
                } else {
                    legacyRows(context.state)
                }
            }
            .padding()
            .activityBackgroundTint(Color.black.opacity(0.85))
            .activitySystemActionForegroundColor(Color.white)
            .widgetURL(WorkActivityStyle.url(context.state.deepLink))
        } dynamicIsland: { context in
            DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    HStack(spacing: 6) {
                        dot(context.state)
                        if let kind = context.state.featuredKind {
                            Image(systemName: WorkActivityStyle.icon(kind))
                                .foregroundColor(.white.opacity(0.8))
                        }
                    }
                    .padding(.leading, 6)
                }
                DynamicIslandExpandedRegion(.trailing) {
                    Text(WorkActivityStyle.header(context.state))
                        .font(.caption2.monospacedDigit())
                        .foregroundColor(context.state.needsYou > 0 ? .orange : .white.opacity(0.8))
                        .lineLimit(1)
                        .minimumScaleFactor(0.7)
                        .padding(.trailing, 6)
                }
                DynamicIslandExpandedRegion(.center) {
                    Text(context.state.hasFeatured
                         ? (context.state.featuredTitle ?? "")
                         : (context.state.headline.isEmpty ? "Magician at work" : context.state.headline))
                        .font(.subheadline.weight(.semibold))
                        .lineLimit(1)
                        .minimumScaleFactor(0.8)
                }
                DynamicIslandExpandedRegion(.bottom) {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(context.state.hasFeatured ? WorkActivityStyle.featuredDetail(context.state) : context.state.detail)
                            .font(.caption)
                            .foregroundColor(context.state.featuredState == "needs_you" ? .orange : .gray)
                            .lineLimit(1)
                        if context.state.hasFeatured {
                            nextRows(context.state, limit: 2)
                        }
                        HStack {
                            Spacer()
                            actionButton(context.state, compact: true)
                        }
                    }
                    .padding(.horizontal, 6)
                }
            } compactLeading: {
                dot(context.state)
            } compactTrailing: {
                if context.state.needsYou > 0 {
                    Text("!")
                        .font(.caption2.weight(.heavy))
                        .foregroundColor(.orange)
                } else {
                    Text("\(context.state.working + context.state.needsYou)")
                        .font(.caption2.monospacedDigit())
                }
            } minimal: {
                dot(context.state)
            }
            .widgetURL(WorkActivityStyle.url(context.state.deepLink))
        }
    }

    private func dot(_ state: Summary) -> some View {
        let color: Color = state.featuredState.map(WorkActivityStyle.color)
            ?? (state.needsYou > 0 ? .orange : .blue)
        return Circle()
            .fill(color)
            .frame(width: 8, height: 8)
    }

    private func featuredRow(_ state: Summary) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: WorkActivityStyle.icon(state.featuredKind ?? ""))
                .font(.subheadline)
                .foregroundColor(WorkActivityStyle.color(state.featuredState ?? "working"))
                .frame(width: 20)
                .padding(.top, 1)
            VStack(alignment: .leading, spacing: 2) {
                Text(state.featuredTitle ?? "")
                    .font(.headline)
                    .foregroundColor(.white)
                    .lineLimit(1)
                let detail = WorkActivityStyle.featuredDetail(state)
                if !detail.isEmpty {
                    Text(detail)
                        .font(.caption)
                        .foregroundColor(state.featuredState == "needs_you" ? .orange : .white.opacity(0.7))
                        .lineLimit(1)
                }
            }
            Spacer(minLength: 0)
        }
    }

    @ViewBuilder
    private func nextRows(_ state: Summary, limit: Int) -> some View {
        let next = Array((state.next ?? []).prefix(limit))
        ForEach(next, id: \.id) { item in
            HStack(spacing: 6) {
                Circle()
                    .fill(WorkActivityStyle.color(item.state))
                    .frame(width: 6, height: 6)
                Image(systemName: WorkActivityStyle.icon(item.kind))
                    .font(.caption2)
                    .foregroundColor(.white.opacity(0.6))
                Text(item.title)
                    .font(.caption)
                    .foregroundColor(.white.opacity(0.85))
                    .lineLimit(1)
            }
        }
        if let more = state.more, more > 0, limit == 2 {
            Text("+\(more) more")
                .font(.caption2)
                .foregroundColor(.white.opacity(0.55))
        }
    }

    /// What an older server sends: a headline and a detail.
    @ViewBuilder
    private func legacyRows(_ state: Summary) -> some View {
        if !state.headline.isEmpty {
            Text(state.headline)
                .font(.headline)
                .foregroundColor(.white)
                .lineLimit(1)
        }
        if !state.detail.isEmpty {
            Text(state.detail)
                .font(.caption)
                .foregroundColor(state.needsYou > 0 ? .orange : .white.opacity(0.7))
                .lineLimit(2)
        }
    }

    /// One button for the featured item. On the lock screen it shows only
    /// when the item asks something (the row itself opens it); expanded, it is
    /// always there.
    @ViewBuilder
    private func actionButton(_ state: Summary, compact: Bool) -> some View {
        WorkActivityActionButton(
            actionKind: state.actionKind,
            actionId: state.actionId,
            deepLink: state.deepLink,
            alwaysShow: compact
        )
    }
}

/// Approve (a run's diff, answered in place by `ApproveRunChangesIntent`),
/// Answer or Open (both open the item in the app).
struct WorkActivityActionButton: View {
    let actionKind: String?
    let actionId: String?
    let deepLink: String?
    /// False: shown only while the item asks the owner something.
    var alwaysShow = true

    var body: some View {
        if actionKind == "approve_diff", let actionId, !actionId.isEmpty {
            Button(intent: ApproveRunChangesIntent(approvalID: actionId)) {
                Label("Approve", systemImage: "checkmark")
                    .font(.caption.weight(.semibold))
            }
            .tint(.green)
        } else if alwaysShow || actionKind != nil, let url = WorkActivityStyle.url(deepLink) {
            Link(destination: url) {
                Label(actionKind == nil ? "Open" : "Answer",
                      systemImage: actionKind == nil ? "arrow.up.forward.app" : "bubble.left")
                    .font(.caption.weight(.semibold))
                    .padding(.horizontal, 10)
                    .padding(.vertical, 5)
                    .background(Capsule().fill(Color.white.opacity(0.18)))
            }
        }
    }
}

/// Shared look of the summary and the followed-item activities.
enum WorkActivityStyle {
    /// SF Symbol per item kind.
    static func icon(_ kind: String) -> String {
        switch kind {
        case "coding_run": return "chevron.left.forwardslash.chevron.right"
        case "crew": return "person.3.sequence.fill"
        case "work_task": return "checklist"
        default: return "sparkles"
        }
    }

    static func color(_ state: String) -> Color {
        switch state {
        case "needs_you": return .orange
        case "working": return .blue
        case "paused": return .yellow
        case "failed": return .red
        case "done": return .green
        default: return .gray
        }
    }

    static func label(_ state: String) -> String {
        switch state {
        case "needs_you": return "Needs you"
        case "working": return "Working"
        case "paused": return "Paused"
        case "failed": return "Failed"
        case "stopped": return "Stopped"
        case "done": return "Done"
        default: return state
        }
    }

    /// "3 running · 1 needs you", zero parts left out.
    static func header(_ state: CodingRunsActivityAttributes.ContentState) -> String {
        var parts: [String] = []
        if state.working > 0 { parts.append("\(state.working) running") }
        if state.needsYou > 0 { parts.append("\(state.needsYou) needs you") }
        if state.failed > 0 { parts.append("\(state.failed) failed") }
        if parts.isEmpty, state.done > 0 { parts.append("\(state.done) done") }
        return parts.joined(separator: " · ")
    }

    /// "Project · step" for the featured item; "Needs you" leads when it waits.
    static func featuredDetail(_ state: CodingRunsActivityAttributes.ContentState) -> String {
        detail(state: state.featuredState ?? "", context: state.featuredContext ?? "", step: state.featuredStep ?? "")
    }

    static func detail(state: String, context: String, step: String) -> String {
        var parts: [String] = []
        if !context.isEmpty { parts.append(context) }
        if state == "needs_you" {
            parts.append(step.isEmpty ? "Needs you" : "Needs you: \(step)")
        } else if !step.isEmpty {
            parts.append(step)
        } else if state != "working" {
            parts.append(label(state))
        }
        return parts.joined(separator: " · ")
    }

    /// The item's link into the app, else VibeDev Now.
    static func url(_ deepLink: String?) -> URL? {
        if let deepLink, !deepLink.isEmpty, let url = URL(string: deepLink) { return url }
        return URL(string: "magican://vibe")
    }
}
