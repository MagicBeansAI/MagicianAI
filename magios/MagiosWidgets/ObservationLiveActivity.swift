import ActivityKit
import SwiftUI
import WidgetKit

/// Live Activity + Dynamic Island for an ongoing observation. Glanceable proof
/// that Magican is capturing (the phone-on-the-table case), with a one-tap Stop and
/// a deep link into the live transcript.
struct ObservationLiveActivity: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: ObservationActivityAttributes.self) { context in
            // Lock screen / banner
            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 8) {
                    Circle()
                        .fill(context.state.phase == "paused" ? Color.orange : Color.red)
                        .frame(width: 9, height: 9)
                    Text(context.state.phase == "paused" ? "Paused" : "Listening")
                        .font(.subheadline.weight(.semibold))
                        .foregroundColor(.white)
                    Image(systemName: iconName(context.attributes.kind))
                        .font(.caption)
                        .foregroundColor(.white.opacity(0.6))
                    Spacer()
                    Text(context.attributes.startedAt, style: .timer)
                        .font(.subheadline.monospacedDigit())
                        .foregroundColor(.white.opacity(0.7))
                        .frame(maxWidth: 58, alignment: .trailing)
                }
                Text(context.attributes.title)
                    .font(.headline)
                    .foregroundColor(.white)
                    .lineLimit(1)
                if !context.state.latestSummary.isEmpty {
                    Text(context.state.latestSummary)
                        .font(.caption)
                        .foregroundColor(.white.opacity(0.7))
                        .lineLimit(2)
                }
                HStack {
                    Link(destination: threadURL(context.attributes.threadId)) {
                        Label("Open", systemImage: "text.bubble").font(.caption)
                    }
                    Spacer()
                    Button(intent: StopObservationIntent()) {
                        Label("Stop", systemImage: "stop.fill").font(.caption.weight(.semibold))
                    }
                    .tint(.red)
                }
                .padding(.top, 2)
            }
            .padding()
            .activityBackgroundTint(Color.black.opacity(0.85))
            .activitySystemActionForegroundColor(Color.white)

        } dynamicIsland: { context in
            DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    HStack(spacing: 6) {
                        Circle()
                            .fill(context.state.phase == "paused" ? Color.orange : Color.red)
                            .frame(width: 8, height: 8)
                        Image(systemName: iconName(context.attributes.kind))
                            .foregroundColor(.white.opacity(0.8))
                    }
                    .padding(.leading, 6)
                }
                DynamicIslandExpandedRegion(.trailing) {
                    Text(context.attributes.startedAt, style: .timer)
                        .font(.caption.monospacedDigit())
                        .foregroundColor(.white.opacity(0.8))
                        .frame(maxWidth: 54)
                }
                DynamicIslandExpandedRegion(.center) {
                    Text(context.attributes.title)
                        .font(.subheadline.weight(.semibold))
                        .lineLimit(1)
                        .minimumScaleFactor(0.8)
                }
                DynamicIslandExpandedRegion(.bottom) {
                    HStack {
                        if context.state.latestSummary.isEmpty {
                            Text(context.state.phase == "paused" ? "Capture paused" : "Recording the meeting")
                                .font(.caption).foregroundColor(.gray).lineLimit(1)
                        } else {
                            Text(context.state.latestSummary)
                                .font(.caption).foregroundColor(.gray).lineLimit(1)
                        }
                        Spacer()
                        Button(intent: StopObservationIntent()) {
                            Label("Stop", systemImage: "stop.fill").font(.caption2.weight(.semibold))
                        }
                        .tint(.red)
                    }
                    .padding(.horizontal, 6)
                }
            } compactLeading: {
                Circle()
                    .fill(context.state.phase == "paused" ? Color.orange : Color.red)
                    .frame(width: 8, height: 8)
            } compactTrailing: {
                Text(context.attributes.startedAt, style: .timer)
                    .font(.caption2.monospacedDigit())
                    .frame(maxWidth: 44)
            } minimal: {
                Circle()
                    .fill(context.state.phase == "paused" ? Color.orange : Color.red)
                    .frame(width: 8, height: 8)
            }
        }
    }

    private func iconName(_ kind: String) -> String {
        kind == "screen" ? "rectangle.on.rectangle" : "waveform"
    }

    private func threadURL(_ thread: String) -> URL {
        URL(string: "magican://thread/\(thread)") ?? URL(string: "magican://observe")!
    }
}
