import ActivityKit
import SwiftUI
import WidgetKit

/// Lock screen + Dynamic Island for one followed item (a coding run, a crew
/// or a Work task), in the "Magician at work" look: kind icon, title,
/// project and step, spend when known, and one button — Approve for a run's
/// diff, else Answer / Open into the app.
struct FollowedItemLiveActivity: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: FollowedItemActivityAttributes.self) { context in
            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 8) {
                    Circle()
                        .fill(WorkActivityStyle.color(context.state.state))
                        .frame(width: 8, height: 8)
                    Text(WorkActivityStyle.label(context.state.state))
                        .font(.caption.weight(.semibold))
                        .foregroundColor(context.state.state == "needs_you" ? .orange : .white.opacity(0.8))
                    Spacer()
                    if let spent = context.state.spentUsd {
                        Text(Self.usd(spent))
                            .font(.caption.monospacedDigit())
                            .foregroundColor(.white.opacity(0.6))
                    }
                }
                HStack(alignment: .center, spacing: 10) {
                    Image(systemName: WorkActivityStyle.icon(context.attributes.kind))
                        .font(.subheadline)
                        .foregroundColor(WorkActivityStyle.color(context.state.state))
                        .frame(width: 20)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(context.state.title)
                            .font(.headline)
                            .foregroundColor(.white)
                            .lineLimit(1)
                        let detail = Self.detail(context.state)
                        if !detail.isEmpty {
                            Text(detail)
                                .font(.caption)
                                .foregroundColor(context.state.state == "needs_you" ? .orange : .white.opacity(0.7))
                                .lineLimit(2)
                        }
                    }
                    Spacer(minLength: 0)
                    WorkActivityActionButton(
                        actionKind: context.state.actionKind,
                        actionId: context.state.actionId,
                        deepLink: context.state.deepLink,
                        alwaysShow: false
                    )
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
                        Circle()
                            .fill(WorkActivityStyle.color(context.state.state))
                            .frame(width: 8, height: 8)
                        Image(systemName: WorkActivityStyle.icon(context.attributes.kind))
                            .foregroundColor(.white.opacity(0.8))
                    }
                    .padding(.leading, 6)
                }
                DynamicIslandExpandedRegion(.trailing) {
                    Text(WorkActivityStyle.label(context.state.state))
                        .font(.caption2.weight(.semibold))
                        .foregroundColor(context.state.state == "needs_you" ? .orange : .white.opacity(0.8))
                        .lineLimit(1)
                        .padding(.trailing, 6)
                }
                DynamicIslandExpandedRegion(.center) {
                    Text(context.state.title)
                        .font(.subheadline.weight(.semibold))
                        .lineLimit(1)
                        .minimumScaleFactor(0.8)
                }
                DynamicIslandExpandedRegion(.bottom) {
                    HStack {
                        Text(Self.detail(context.state))
                            .font(.caption)
                            .foregroundColor(.gray)
                            .lineLimit(1)
                        Spacer()
                        WorkActivityActionButton(
                            actionKind: context.state.actionKind,
                            actionId: context.state.actionId,
                            deepLink: context.state.deepLink
                        )
                    }
                    .padding(.horizontal, 6)
                }
            } compactLeading: {
                Image(systemName: WorkActivityStyle.icon(context.attributes.kind))
                    .foregroundColor(WorkActivityStyle.color(context.state.state))
            } compactTrailing: {
                if context.state.state == "needs_you" {
                    Text("!")
                        .font(.caption2.weight(.heavy))
                        .foregroundColor(.orange)
                } else {
                    Circle()
                        .fill(WorkActivityStyle.color(context.state.state))
                        .frame(width: 8, height: 8)
                }
            } minimal: {
                Circle()
                    .fill(WorkActivityStyle.color(context.state.state))
                    .frame(width: 8, height: 8)
            }
            .widgetURL(WorkActivityStyle.url(context.state.deepLink))
        }
    }

    private static func detail(_ state: FollowedItemActivityAttributes.ContentState) -> String {
        WorkActivityStyle.detail(state: state.state, context: state.context, step: state.step)
    }

    private static func usd(_ value: Double) -> String {
        value < 1 ? String(format: "$%.2f", value) : String(format: "$%.1f", value)
    }
}
