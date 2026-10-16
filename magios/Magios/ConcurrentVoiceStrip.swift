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
    /// Measured in the list's own content space: total height and each row's
    /// bottom edge. The list's frame is set from these, never left to the
    /// parent's leftover space.
    @State private var listContentHeight: CGFloat = 0
    @State private var listRowBottoms: [Int: CGFloat] = [:]
    @State private var listBottomY: CGFloat?

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
                                .background(GeometryReader { proxy in
                                    Color.clear.preference(key: VoiceRequestRowBottomsKey.self,
                                        value: [index: proxy.frame(in: .named(Self.listSpace)).maxY])
                                })
                            }
                        }.padding(.vertical, Self.listPadding).padding(.horizontal, 12)
                        .coordinateSpace(.named(Self.listSpace))
                        .onPreferenceChange(VoiceRequestRowBottomsKey.self) { listRowBottoms = $0 }
                        .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { listContentHeight = $0 }
                    }
                    // A fixed height, not a max: a bare max let the chat's VStack
                    // split leftover space between this list and the transcript,
                    // so with the keyboard up the list got one row.
                    .frame(height: listHeight)
                    .scrollIndicators(.visible)
                    .scrollIndicatorsFlash(onAppear: true)
                    .scrollIndicatorsFlash(trigger: coordinator.available.count)
                    // The list's bottom edge stays put as it grows (the card is
                    // bottom-anchored), so it measures the room above without a loop.
                    .onGeometryChange(for: CGFloat.self) { $0.frame(in: .global).maxY } action: { listBottomY = $0 }
                    .accessibilityIdentifier("voice-requests-list")
                }
            }
            // The error sits inside the strip, above its hairline, on the
            // same 14 pt inset as the header text.
            if let error = coordinator.error {
                Text(error)
                    .font(.themed(11))
                    .foregroundColor(theme.dangerColor)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 6)
                    .accessibilityIdentifier("voice-request-error")
            }
            if coordinator.available.first != nil || coordinator.error != nil {
                Rectangle().fill(theme.secondaryTextColor.opacity(0.2)).frame(height: 0.5).accessibilityHidden(true)
            }
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
    private static let listSpace = "voice-requests-list"
    private static let listPadding: CGFloat = 6
    private var listHeight: CGFloat {
        let count = coordinator.available.count
        let bottoms = (0..<count).compactMap { listRowBottoms[$0] }
        return ConcurrentVoiceListSizing.height(
            contentHeight: listContentHeight,
            rowBottoms: bottoms.count == count ? bottoms : [],
            available: listBottomY,
            bottomPadding: Self.listPadding)
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

/// How tall the expanded request list is: its content, up to four rows plus a
/// peek of the fifth (so it reads as scrollable), and never more than 40% of
/// the room above its bottom edge. Rows are measured, so one- and two-line
/// rows both count as one row.
enum ConcurrentVoiceListSizing {
    static let visibleRows = 4
    static let peek: CGFloat = 18
    static let availableFraction: CGFloat = 0.4

    /// - Parameters:
    ///   - contentHeight: the list's full content height, padding included.
    ///   - rowBottoms: each row's bottom edge from the content top, in order;
    ///     empty while rows are unmeasured.
    ///   - available: the room above the list's bottom edge, when known.
    ///   - bottomPadding: the content's padding below the last row.
    static func height(contentHeight: CGFloat, rowBottoms: [CGFloat], available: CGFloat?,
                       bottomPadding: CGFloat) -> CGFloat {
        guard contentHeight > 0 else { return 0 }
        var cap = contentHeight
        if rowBottoms.count > visibleRows {
            cap = rowBottoms[visibleRows - 1] + peek
        }
        if let available, available > 0 {
            // Short screens: a share of the room, but never less than one row.
            let floor = rowBottoms.first.map { $0 + bottomPadding } ?? 0
            cap = min(cap, max(available * availableFraction, floor))
        }
        return min(contentHeight, cap)
    }
}

private struct VoiceRequestRowBottomsKey: PreferenceKey {
    static var defaultValue: [Int: CGFloat] = [:]
    static func reduce(value: inout [Int: CGFloat], nextValue: () -> [Int: CGFloat]) {
        value.merge(nextValue()) { $1 }
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
