import SwiftUI

/// Observe → Audio: the transcription profile for the two observation surfaces
/// ("meeting" and "listening"), read from and saved to
/// `/api/magician/v2/media/preferences` (profiles from `/media/providers`).
/// Selecting a row saves immediately (optimistic, rolled back on failure).
struct ObserveAudioPane: View {
    @ObservedObject var audio: ObserveAudioProfilesViewModel
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            ObserveSectionHeader(title: "Audio profiles") {
                if audio.loading { ProgressView().scaleEffect(0.8) }
            }
            Text("Which speech-to-text pipeline transcribes each kind of capture. Changes apply to the next capture.")
                .font(.caption)
                .foregroundColor(theme.secondaryTextColor)
            if let err = audio.loadError {
                ObserveBanner(text: err, color: theme.dangerColor) { Task { await audio.reload() } }
            }
            if let err = audio.saveError {
                ObserveBanner(text: err, color: theme.dangerColor)
            }
            if audio.loaded {
                ForEach(ObserveAudioSurface.allCases) { surfaceCard($0) }
            } else if audio.loadError == nil {
                HStack(spacing: 8) {
                    ProgressView().scaleEffect(0.8)
                    Text("Loading audio profiles…").font(.subheadline).foregroundColor(theme.secondaryTextColor)
                }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("observe-audio")
    }

    private func surfaceCard(_ surface: ObserveAudioSurface) -> some View {
        let selected = audio.selections[surface]
        let defaultId = audio.catalog.defaults[surface]
        let activeId = audio.activeProfileId(for: surface)
        let profiles = audio.profiles(for: surface)
        return ObserveCard(spacing: 10) {
            HStack(spacing: 10) {
                Image(systemName: surface == .meeting ? "person.2.wave.2.fill" : "ear.badge.waveform")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundColor(theme.discoveryColor)
                    .frame(width: 28, height: 28)
                    .background(theme.discoveryColor.opacity(0.13))
                    .clipShape(RoundedRectangle(cornerRadius: 7))
                VStack(alignment: .leading, spacing: 1) {
                    Text(surface.label).font(.subheadline.weight(.semibold)).foregroundColor(theme.textColor)
                    Text(surface.detail).font(.caption2).foregroundColor(theme.secondaryTextColor)
                }
                Spacer(minLength: 4)
                if audio.saving == surface { ProgressView().scaleEffect(0.7) }
            }
            Text("Current: \(activeId.map(ObserveAudioProfile.label(for:)) ?? "Server default")\(selected == nil ? " (default)" : "")")
                .font(.caption.weight(.semibold))
                .foregroundColor(theme.accentColor)
                .accessibilityIdentifier("observe-audio-current-\(surface.rawValue)")
            VStack(spacing: 0) {
                optionRow(
                    title: "Use configured default",
                    detail: defaultId.map { "Currently \(ObserveAudioProfile.label(for: $0))" } ?? "The server picks the profile",
                    checked: selected == nil,
                    surface: surface
                ) { Task { await audio.select(nil, for: surface) } }
                ForEach(profiles) { p in
                    Divider()
                    optionRow(title: p.label, detail: p.summary, checked: selected == p.id, surface: surface) {
                        Task { await audio.select(p.id, for: surface) }
                    }
                }
            }
            .background(theme.controlColor.opacity(0.45))
            .clipShape(RoundedRectangle(cornerRadius: 10))
            if profiles.isEmpty {
                Text("No other \(surface.label.lowercased()) profiles are configured on the server.")
                    .font(.caption2)
                    .foregroundColor(theme.secondaryTextColor)
            }
        }
    }

    private func optionRow(
        title: String,
        detail: String,
        checked: Bool,
        surface: ObserveAudioSurface,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            HStack(spacing: 10) {
                Image(systemName: checked ? "checkmark.circle.fill" : "circle")
                    .foregroundColor(checked ? theme.accentColor : theme.secondaryTextColor.opacity(0.6))
                VStack(alignment: .leading, spacing: 1) {
                    Text(title).font(.subheadline).foregroundColor(theme.textColor)
                    Text(detail).font(.caption2).foregroundColor(theme.secondaryTextColor).lineLimit(2)
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 9)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(audio.saving != nil)
        .accessibilityAddTraits(checked ? .isSelected : [])
        .accessibilityLabel("\(surface.label) profile: \(title)")
    }
}
