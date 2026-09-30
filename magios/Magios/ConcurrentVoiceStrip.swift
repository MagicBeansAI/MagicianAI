import SwiftUI
import MarkdownUI

struct ConcurrentVoiceStrip: View {
    @ObservedObject var coordinator: ConcurrentVoiceCoordinator
    var onReview: (String) -> Void
    var topCornerRadius: CGFloat = ComposerView.cornerRadius
    @ObservedObject private var theme = ThemeManager.shared
    @State private var expanded = false
    @State private var result: VoiceRequestRecord?
    @State private var resultText = ""
    @State private var headerPressed = false
    @State private var headerHovered = false

    var body: some View {
        VStack(spacing: 0) {
            if let latest = coordinator.available.first {
                HStack(alignment: .center, spacing: 0) {
                    Button { expanded.toggle() } label: {
                        HStack(alignment: .center, spacing: 7) {
                            Image(systemName: "chevron.down")
                                .font(.system(size: 10, weight: .semibold))
                                .frame(width: 14, height: 14, alignment: .center)
                                .rotationEffect(.degrees(expanded ? 0 : 180))
                            Text(status(latest)).fontWeight(.semibold).fixedSize()
                            Text(latest.title).lineLimit(1).truncationMode(.tail).frame(maxWidth: .infinity, alignment: .leading)
                            Text("\(coordinator.available.count)").monospacedDigit().fixedSize()
                        }
                        .font(.themed(12)).lineLimit(1)
                        .frame(minHeight: 30, alignment: .center)
                        .padding(.leading, 14).padding(.trailing, 4)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(VoiceHeaderButtonStyle())
                    .accessibilityLabel("\(expanded ? "Collapse" : "Expand") background requests (\(coordinator.available.count))")
                    .accessibilityIdentifier("voice-requests-summary")
                    .accessibilityValue(status(latest))
                    options(latest, header: true)
                }
                .foregroundColor(theme.secondaryTextColor)
                .background(theme.accentColor.opacity(headerPressed || headerHovered ? 0.12 : 0))
                .clipShape(UnevenRoundedRectangle(topLeadingRadius: topCornerRadius, topTrailingRadius: topCornerRadius))
                .onPreferenceChange(VoiceHeaderPressedKey.self) { headerPressed = $0 }
                .onHover { headerHovered = $0 }
                if expanded {
                    ScrollView {
                        VStack(alignment: .leading, spacing: 0) {
                            if let focus = coordinator.focus {
                                HStack {
                                    Text("Next voice topic: \(focus.title)").lineLimit(1)
                                    Spacer()
                                    Button("New topic") { coordinator.select(nil) }
                                }.font(.themed(11)).foregroundColor(theme.secondaryTextColor).padding(.bottom, 8)
                            }
                            ForEach(Array(coordinator.available.enumerated()), id: \.element.id) { index, row in
                                if index > 0 {
                                    Rectangle().fill(theme.secondaryTextColor.opacity(0.2))
                                        .frame(height: 0.5).accessibilityHidden(true)
                                }
                                HStack {
                                    Button { coordinator.select(row) } label: {
                                        VStack(alignment: .leading, spacing: 3) {
                                            Text(status(row)).font(.themed(11, weight: .semibold)).foregroundColor(theme.secondaryTextColor)
                                            Text(row.title).font(.themed(12)).lineLimit(2).foregroundColor(theme.textColor)
                                            if let error = row.error { Text(error).font(.themed(11)).foregroundColor(theme.secondaryTextColor) }
                                        }.frame(maxWidth: .infinity, alignment: .leading)
                                    }.buttonStyle(.plain)
                                    options(row)
                                }.padding(.vertical, 6)
                            }
                        }.padding(.vertical, 6).padding(.horizontal, 12)
                    }.frame(maxHeight: 220)
                }
                Divider().overlay(theme.secondaryTextColor.opacity(0.2))
            }
            if let error = coordinator.error { Text(error).font(.themed(11)).foregroundColor(theme.secondaryTextColor).accessibilityIdentifier("voice-request-error") }
        }
        .sheet(item: $result) { row in
            NavigationStack {
                ScrollView { Markdown(resultText).textSelection(.enabled).padding() }
                    .background(theme.backgroundColor).foregroundColor(theme.textColor)
                    .navigationTitle("Voice result")
                    .toolbar {
                        ToolbarItem(placement: .cancellationAction) { Button("Done") { result = nil } }
                        ToolbarItem(placement: .primaryAction) { Button("Review work") { result = nil; onReview(row.branchSessionId) } }
                    }
            }
        }
    }
    private func status(_ row: VoiceRequestRecord) -> String { coordinator.speaking == row.id ? "Speaking" : row.label }
    private func options(_ row: VoiceRequestRecord, header: Bool = false) -> some View {
        Menu {
            Button("Continue this topic") { coordinator.select(row) }
            if row.running { Button("Cancel request", role: .destructive) { coordinator.action { try await coordinator.cancel(row.id) } } }
            else if row.speechText != nil { Button("Read aloud") { coordinator.replay(row.id) } }
            if row.resultMessageId != nil {
                Button("View result") {
                    result = row; resultText = "Loading…"
                    coordinator.action {
                        let text = try await coordinator.result(row.id)
                        if result?.id == row.id { resultText = text; try await coordinator.markRead(row.id) }
                    }
                }
            }
            Button("Review work") { onReview(row.branchSessionId) }
            if !["accepted", "running"].contains(row.workStatus) { Button("Dismiss") { coordinator.action { try await coordinator.dismiss(row.id) } } }
            Button("New topic") { coordinator.select(nil) }
        } label: {
            Image(systemName: "ellipsis").font(.system(size: 15, weight: .semibold)).frame(width: header ? 44 : 32, height: header ? 30 : 24, alignment: .center)
        }
        .buttonStyle(VoiceHeaderButtonStyle())
        .foregroundColor(theme.secondaryTextColor)
        .accessibilityLabel("Options for \(row.title)")
    }
}

private struct VoiceHeaderPressedKey: PreferenceKey {
    static var defaultValue = false
    static func reduce(value: inout Bool, nextValue: () -> Bool) { value = value || nextValue() }
}

/// The enclosing header paints the press state across both controls and its corners.
private struct VoiceHeaderButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label.preference(key: VoiceHeaderPressedKey.self, value: configuration.isPressed)
    }
}

/// A full-width composer header; even the empty space remains tappable.
struct ComposerTopRow: View {
    var title: String
    var systemImage: String
    var topCornerRadius: CGFloat = ComposerView.cornerRadius
    var action: () -> Void
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        VStack(spacing: 0) {
            Button(action: action) {
                HStack(alignment: .center, spacing: 7) {
                    Text(title).lineLimit(1).truncationMode(.tail)
                    Spacer(minLength: 0)
                    Image(systemName: systemImage).font(.system(size: 10, weight: .semibold))
                        .frame(width: 14, height: 14, alignment: .center)
                }
                .font(.themed(12)).foregroundColor(theme.secondaryTextColor)
                .padding(.horizontal, 14)
                .frame(maxWidth: .infinity, minHeight: 30, alignment: .center)
                .contentShape(Rectangle())
            }
            .buttonStyle(ComposerTopRowButtonStyle(accent: theme.accentColor, cornerRadius: topCornerRadius))
            Rectangle().fill(theme.secondaryTextColor.opacity(0.2)).frame(height: 0.5).accessibilityHidden(true)
        }
    }
}

private struct ComposerTopRowButtonStyle: ButtonStyle {
    var accent: Color
    var cornerRadius: CGFloat
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .background(accent.opacity(configuration.isPressed ? 0.12 : 0))
            .clipShape(UnevenRoundedRectangle(topLeadingRadius: cornerRadius, topTrailingRadius: cornerRadius))
    }
}
