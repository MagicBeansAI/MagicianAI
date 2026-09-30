import WidgetKit
import SwiftUI
import AppIntents

struct MagiosWidgetEntry: TimelineEntry {
    let date: Date
    let snapshot: MagicanGlanceSnapshot
}

struct Provider: TimelineProvider {
    func placeholder(in context: Context) -> MagiosWidgetEntry {
        MagiosWidgetEntry(
            date: Date(),
            snapshot: MagicanGlanceSnapshot(
                generatedAt: Int64(Date().timeIntervalSince1970 * 1_000),
                focus: .activeWork,
                title: "Preparing your briefing",
                subtitle: "Working…",
                needsYouCount: 1,
                activeWorkCount: 2,
                taskID: "preview"
            )
        )
    }

    func getSnapshot(in context: Context, completion: @escaping (MagiosWidgetEntry) -> Void) {
        completion(MagiosWidgetEntry(
            date: Date(),
            snapshot: MagicanGlanceCache.load() ?? .ready
        ))
    }

    func getTimeline(in context: Context, completion: @escaping (Timeline<MagiosWidgetEntry>) -> Void) {
        Task {
            let snapshot = await MagicanGlanceLoader.refresh()
            let now = Date()
            completion(Timeline(
                entries: [MagiosWidgetEntry(date: now, snapshot: snapshot)],
                policy: .after(now.addingTimeInterval(15 * 60))
            ))
        }
    }
}

private enum MagicanGlanceLoader {
    static func refresh() async -> MagicanGlanceSnapshot {
        let fallback = MagicanGlanceCache.load() ?? .ready
        guard MagicianAccess.isConfigured else { return fallback }
        var components = URLComponents(
            url: MagicianAccess.baseURL
                .appendingPathComponent("api/magician/v2/today"),
            resolvingAgainstBaseURL: false
        )
        components?.queryItems = [
            URLQueryItem(name: "per_section", value: "1"),
            URLQueryItem(name: "digest_limit", value: "1")
        ]
        guard let url = components?.url else { return fallback }
        var request = URLRequest(url: url)
        request.timeoutInterval = 12
        request.setValue("no-cache", forHTTPHeaderField: "Cache-Control")
        MagicianAccess.authorize(&request)
        do {
            let (data, response) = try await URLSession.shared.data(for: request)
            guard let http = response as? HTTPURLResponse,
                  (200..<300).contains(http.statusCode) else { return fallback }
            let snapshot = try MagicanGlanceSnapshot.reducingToday(data)
            MagicanGlanceCache.save(snapshot)
            return MagicanGlanceCache.load() ?? snapshot
        } catch {
            return fallback
        }
    }
}

struct MagiosWidgetView: View {
    var entry: Provider.Entry
    @Environment(\.widgetFamily) private var family

    var body: some View {
        content
            .widgetURL(entry.snapshot.destinationURL)
    }

    @ViewBuilder
    private var content: some View {
        switch family {
        case .accessoryCircular:
            if entry.snapshot.focus == .ready {
                Button(intent: ArmAmbientIntent()) {
                    ZStack {
                        AccessoryWidgetBackground()
                        Image(systemName: "waveform.circle.fill")
                            .font(.system(size: 27, weight: .semibold))
                            .widgetAccentable()
                    }
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Talk to Magican")
            } else {
                ZStack {
                    AccessoryWidgetBackground()
                    Image(systemName: focusIcon)
                        .font(.system(size: 27, weight: .semibold))
                        .widgetAccentable()
                }
                .accessibilityLabel(entry.snapshot.title)
                .accessibilityHint("Opens the item shown by Magican")
            }

        case .accessoryRectangular:
            HStack(spacing: 9) {
                Image(systemName: focusIcon)
                    .font(.system(size: 29, weight: .semibold))
                    .widgetAccentable()
                VStack(alignment: .leading, spacing: 1) {
                    Text(entry.snapshot.title)
                        .font(.system(size: 14, weight: .bold, design: .rounded))
                        .lineLimit(1)
                    Text(entry.snapshot.subtitle)
                        .font(.system(size: 11, weight: .medium, design: .rounded))
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                Spacer(minLength: 0)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)

        default:
            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    Text("MAGICAN")
                        .font(.system(size: 9, weight: .bold, design: .rounded))
                        .tracking(1.1)
                        .foregroundStyle(.white.opacity(0.78))
                    Spacer()
                    if entry.snapshot.needsYouCount > 0 {
                        Text("\(entry.snapshot.needsYouCount) NEEDS YOU")
                            .font(.system(size: 8, weight: .bold, design: .rounded))
                            .foregroundStyle(.white.opacity(0.88))
                    } else if entry.snapshot.activeWorkCount > 0 {
                        Text("\(entry.snapshot.activeWorkCount) ACTIVE")
                            .font(.system(size: 8, weight: .bold, design: .rounded))
                            .foregroundStyle(.white.opacity(0.88))
                    }
                }

                HStack(alignment: .center, spacing: 10) {
                    ZStack {
                        Circle()
                            .fill(.white.opacity(0.14))
                            .frame(width: 48, height: 48)
                        Circle()
                            .stroke(.white.opacity(0.22), lineWidth: 1)
                            .frame(width: 57, height: 57)
                        Image(systemName: focusIcon)
                            .font(.system(size: 22, weight: .bold))
                            .foregroundStyle(.white)
                    }
                    VStack(alignment: .leading, spacing: 3) {
                        Text(entry.snapshot.title)
                            .font(.system(size: 15, weight: .bold, design: .rounded))
                            .foregroundStyle(.white)
                            .lineLimit(2)
                        Text(entry.snapshot.subtitle)
                            .font(.system(size: 10, weight: .medium, design: .rounded))
                            .foregroundStyle(.white.opacity(0.76))
                            .lineLimit(1)
                    }
                }

                Spacer(minLength: 0)
                HStack {
                    Text(entry.snapshot.focus == .ready ? "Tap to talk" : "Tap to open")
                        .font(.system(size: 9, weight: .semibold, design: .rounded))
                        .foregroundStyle(.white.opacity(0.72))
                    Spacer()
                    Button(intent: ArmAmbientIntent()) {
                        Image(systemName: "waveform.circle.fill")
                            .font(.system(size: 24, weight: .semibold))
                            .foregroundStyle(.white)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Talk to Magican")
                    .accessibilityHint("Starts talking now, then waits for wake-word follow-ups")
                }
            }
            .padding(14)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
        }
    }

    private var focusIcon: String {
        switch entry.snapshot.focus {
        case .needsYou: "person.crop.circle.badge.exclamationmark"
        case .activeWork: "sparkles"
        case .ready: "waveform"
        }
    }
}

private struct TalkWidgetBackground: View {
    var body: some View {
        ZStack {
            LinearGradient(
                colors: [
                    Color(red: 0.42, green: 0.16, blue: 0.96),
                    Color(red: 0.96, green: 0.22, blue: 0.46),
                    Color(red: 1.00, green: 0.42, blue: 0.42)
                ],
                startPoint: .topLeading,
                endPoint: .bottomTrailing
            )
            Circle()
                .fill(.white.opacity(0.13))
                .frame(width: 130, height: 130)
                .offset(x: 62, y: -58)
            Circle()
                .fill(.black.opacity(0.08))
                .frame(width: 110, height: 110)
                .offset(x: -68, y: 72)
        }
    }
}

struct MagiosWidgets: Widget {
    let kind: String = "MagiosWidget"

    var body: some WidgetConfiguration {
        StaticConfiguration(kind: kind, provider: Provider()) { entry in
            MagiosWidgetView(entry: entry)
                .containerBackground(for: .widget) {
                    TalkWidgetBackground()
                }
        }
        .configurationDisplayName("Magican at a glance")
        .description("See what needs you and what is active, or start talking.")
        .supportedFamilies([.systemSmall, .systemMedium, .accessoryCircular, .accessoryRectangular])
    }
}

/// The one Control Center / Lock Screen / Action Button voice control.
///
/// **Its absence was a gap, not a decision.** `ArmAmbientIntent` was written and
/// documented as reachable "from Control Center, the Lock Screen, the Action
/// Button, or Shortcuts", and the first three of those require exactly this — a
/// `ControlWidget` — which nothing declared. Only Shortcuts could reach it. Since
/// the whole shape of ambient mode is *one visible launch per window, and every
/// later interaction invisible*, the entry point being buried in Shortcuts made
/// the intent path effectively undiscoverable, and made a first-run prompt
/// pointing at Control Center a promise about something that was not there.
///
/// A system control needs an `OpenIntent` whose target membership covers this
/// extension AND the containing app, which `project.yml` gives
/// `ArmAmbientIntent`. The explicit tap begins the first turn immediately; after
/// it ends, the same ambient window waits for a wake phrase.
@available(iOSApplicationExtension 18.0, *)
struct MagiosAmbientControl: ControlWidget {
    let kind = SharedActions.ambientControlKind

    var body: some ControlWidgetConfiguration {
        StaticControlConfiguration(kind: kind) {
            ControlWidgetButton(action: ArmAmbientIntent()) {
                // This is one custom SF Symbol, not two overlaid views. Control
                // Center flattens a composite icon to its primary symbol, which
                // made the microphone disappear on-device. The asset's one
                // glyph contains both the branded u and microphone outlines.
                Label("Talk to Magican", image: "MagicanTalkControl")
            }
        }
        .displayName("Talk to Magican")
        .description("Start talking now, then use your wake phrase for follow-ups.")
    }
}

@main
struct MagiosWidgetBundle: WidgetBundle {
    var body: some Widget {
        MagiosWidgets()
        MagiosLiveActivity()
        ObservationLiveActivity()
        AmbientLiveActivity()
        if #available(iOSApplicationExtension 18.0, *) {
            MagiosAmbientControl()
        }
    }
}
