import ActivityKit
import WidgetKit
import SwiftUI

struct MagiosLiveActivity: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: MagicianTaskAttributes.self) { context in
            // Lock screen / banner UI
            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    TaskOrbGlyph(state: context.state, diameter: 22)
                    Text(context.state.cardTitle.isEmpty ? context.attributes.taskName : context.state.cardTitle)
                        .font(.headline)
                        .foregroundColor(.white)
                    Spacer()
                    if context.state.isDone {
                        Text("Done")
                            .font(.caption)
                            .foregroundColor(.green)
                            .bold()
                    } else if context.isStale {
                        Text("Paused")
                            .font(.caption.bold())
                            .foregroundColor(.orange)
                    } else {
                        ProgressView()
                            .tint(AuroraPalette.controlAccent)
                    }
                }
                
                HStack {
                    Text(context.isStale ? "Open Magican to check progress" : context.state.status)
                        .font(.subheadline)
                        .foregroundColor(.white.opacity(0.8))
                        .lineLimit(1)
                    
                    Spacer()
                    
                    if context.state.stepCount > 0 {
                        Text("\(context.state.stepCount) step\(context.state.stepCount == 1 ? "" : "s")")
                            .font(.caption)
                            .foregroundColor(.white.opacity(0.6))
                    }
                }
            }
            .padding()
            .activityBackgroundTint(Color.black.opacity(0.8))
            .activitySystemActionForegroundColor(AuroraPalette.controlAccent)

        } dynamicIsland: { context in
            DynamicIsland {
                // Expanded UI
                DynamicIslandExpandedRegion(.leading) {
                    TaskOrbGlyph(state: context.state, diameter: 22)
                        .padding(.leading, 8)
                        .padding(.top, 8)
                }
                DynamicIslandExpandedRegion(.trailing) {
                    if context.state.isDone {
                        Text("Done")
                            .font(.caption)
                            .foregroundColor(.green)
                            .padding(.trailing, 8)
                            .padding(.top, 8)
                    } else if context.isStale {
                        Image(systemName: "exclamationmark.circle.fill")
                            .foregroundColor(.orange)
                            .padding(.trailing, 8)
                            .padding(.top, 8)
                    } else {
                        ProgressView()
                            .tint(AuroraPalette.controlAccent)
                            .padding(.trailing, 8)
                            .padding(.top, 8)
                    }
                }
                DynamicIslandExpandedRegion(.center) {
                    Text(context.state.cardTitle.isEmpty ? context.attributes.taskName : context.state.cardTitle)
                        .font(.headline)
                        .minimumScaleFactor(0.8)
                        .lineLimit(1)
                }
                DynamicIslandExpandedRegion(.bottom) {
                    HStack {
                        Text(context.isStale ? "Open Magican to check progress" : context.state.status)
                            .font(.subheadline)
                            .lineLimit(1)
                        Spacer()
                        if context.state.stepCount > 0 {
                            Text("\(context.state.stepCount) step\(context.state.stepCount == 1 ? "" : "s")")
                                .font(.caption2)
                                .foregroundColor(.gray)
                        }
                    }
                    .padding(.horizontal, 8)
                    .padding(.bottom, 8)
                }
            } compactLeading: {
                TaskOrbGlyph(state: context.state, diameter: 14)
            } compactTrailing: {
                if context.state.isDone {
                    // The orb already sits in compactLeading — a second teal orb
                    // here would just be a twin, so done keeps a checkmark in the
                    // Done-text green.
                    Image(systemName: "checkmark")
                        .font(.caption2.weight(.bold))
                        .foregroundColor(.green)
                } else if context.isStale {
                    Image(systemName: "exclamationmark")
                        .font(.caption2.weight(.bold))
                        .foregroundColor(.orange)
                } else {
                    ProgressView().tint(AuroraPalette.controlAccent)
                }
            } minimal: {
                // Orb only: no "Done" text fits this slot, so the teal palette
                // alone is the visible done signal — mirroring how the ambient
                // orb's graphite-alone signals ended.
                TaskOrbGlyph(state: context.state, diameter: 14)
            }
        }
    }
}

/// The task orb: one home for the state→aurora mapping, the completion ease,
/// and the VoiceOver label — the minimal slot's ONLY channels are here. It
/// speaks the same aurora as the ambient orb — calm violet while running,
/// teal-green when done; `isActive` drives the halo pulse while work is under
/// way.
private struct TaskOrbGlyph: View {
    let state: MagicianTaskAttributes.ContentState
    let diameter: CGFloat

    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        AuroraOrbView(palette: state.isDone ? .tealDone : .calmAurora,
                      diameter: diameter,
                      isActive: !state.isDone)
            // The done flip is a same-identity palette change, which the
            // device pass proved hard-cuts out-of-process — explicit easing on
            // it is ignored. So the orb swaps identity with the flip and the
            // system cross-dissolves the violet-out/teal-in pair, the one fade
            // it honours. Kept under Reduce Motion: a dissolve is the reduced
            // form, not motion. Colour still carries the change on its own.
            .id(state.isDone)
            // Demoted from mechanism to the ≤2s timing hint iOS 17+ may honor
            // on that swap; same-identity easing animates nothing by itself
            // out here. Reduce Motion nils the hint, matching the ambient
            // glyph.
            .animation(reduceMotion ? nil : .auroraPhaseEase, value: state.isDone)
            .accessibilityLabel(state.isDone ? "Done" : state.status)
    }
}
