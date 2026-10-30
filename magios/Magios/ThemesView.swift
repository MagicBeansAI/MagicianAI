import SwiftUI

/// Themes, opened from the side menu: the System / Day / Night switch and every
/// theme family with a live palette preview. Picking a family applies it at
/// once and keeps the sheet open, so themes can be compared in place.
struct ThemesView: View {
    @ObservedObject private var themeManager = ThemeManager.shared
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List {
                Section {
                    AppearanceModePicker(
                        selection: themeManager.appearanceMode,
                        select: { themeManager.setAppearanceMode($0, systemDark: ThemeManager.systemIsDark) }
                    )
                    .listRowBackground(themeManager.surfaceColor)
                } header: {
                    Text("Appearance").foregroundColor(themeManager.secondaryTextColor)
                } footer: {
                    Text("System follows your device's Day/Night. The \(ProductIdentity.productName) keyboard always follows the device appearance, regardless of this.")
                        .foregroundColor(themeManager.secondaryTextColor)
                }

                Section {
                    ForEach(themeManager.availableThemeFamilies) { family in
                        Button {
                            themeManager.applyThemeFamily(family.id, systemDark: ThemeManager.systemIsDark)
                        } label: {
                            HStack(spacing: 12) {
                                ThemePaletteSwatch(
                                    palette: themeManager.previewPalette(
                                        for: family,
                                        systemDark: ThemeManager.systemIsDark
                                    )
                                )
                                Text(family.name)
                                    .foregroundColor(themeManager.textColor)
                                Spacer()
                                if family.id == themeManager.currentThemeFamilyID {
                                    Image(systemName: "checkmark.circle.fill")
                                        .foregroundColor(themeManager.accentColor)
                                }
                            }
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .listRowBackground(themeManager.surfaceColor)
                        .accessibilityLabel(family.name)
                        .accessibilityValue(family.id == themeManager.currentThemeFamilyID ? "Selected" : "")
                    }
                } header: {
                    Text("Theme").foregroundColor(themeManager.secondaryTextColor)
                } footer: {
                    Text(
                        themeManager.appearanceMode == .system
                            ? "Previews follow your device's current appearance."
                            : "Previews show each theme's \(themeManager.appearanceMode.label.lowercased()) palette."
                    )
                    .foregroundColor(themeManager.secondaryTextColor)
                }
            }
            .scrollContentBackground(.hidden)
            .background(themeManager.washGradient.ignoresSafeArea())
            .navigationTitle("Themes")
            .navigationBarTitleDisplayMode(.inline)
            .toolbarBackground(themeManager.backgroundColor, for: .navigationBar)
            .toolbarBackground(.visible, for: .navigationBar)
            .toolbarColorScheme(themeManager.colorScheme, for: .navigationBar)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                        .foregroundColor(themeManager.accentColor)
                }
            }
        }
        // A sheet's navigation bar resolves its SwiftUI toolbar preferences once
        // per presentation, so a live palette flip made INSIDE this sheet (the
        // System/Day/Night switch) leaves the bar on the old palette until a
        // relaunch. Rebuilding the stack on `themeRevision` recreates the bar
        // from the already-updated UIKit appearance, like the working tabs do.
        .id(themeManager.themeRevision)
    }
}

/// System / Day / Night as one themed segment — the twin of magdroid's
/// `AppearanceModePicker`. The stock `.segmented` control only flips light/dark
/// with the color scheme; it never takes the palette, so the switch sat on
/// system grays in every theme.
private struct AppearanceModePicker: View {
    @ObservedObject private var theme = ThemeManager.shared
    let selection: ThemeManager.AppearanceMode
    let select: (ThemeManager.AppearanceMode) -> Void

    var body: some View {
        HStack(spacing: 0) {
            ForEach(Array(ThemeManager.AppearanceMode.allCases.enumerated()), id: \.element.id) { index, mode in
                let chosen = mode == selection
                Button {
                    select(mode)
                } label: {
                    Text(mode.label)
                        .font(.themed(12, weight: chosen ? .semibold : .regular))
                        .foregroundColor(chosen ? theme.onAccentColor : theme.secondaryTextColor)
                        .frame(maxWidth: .infinity, minHeight: 32)
                        .background(
                            RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous)
                                .fill(chosen ? theme.accentFillColor : Color.clear)
                        )
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(mode.label)
                .accessibilityAddTraits(chosen ? [.isSelected] : [])
                if index != ThemeManager.AppearanceMode.allCases.count - 1 {
                    Rectangle()
                        .fill(theme.controlBorderColor.opacity(0.5))
                        .frame(width: 1, height: 18)
                }
            }
        }
        .frame(maxWidth: .infinity)
        .background(
            RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous)
                .fill(theme.softBackgroundColor)
        )
        .overlay(
            RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous)
                .strokeBorder(theme.controlBorderColor, lineWidth: 1)
        )
        .clipShape(RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous))
    }
}

/// A truthful miniature of the palette: its real background, accent and text.
/// Decorative only; the adjacent family name carries the accessible identity.
struct ThemePaletteSwatch: View {
    let palette: ThemeManager.ThemePalette

    var body: some View {
        HStack(spacing: 4) {
            Capsule()
                .fill(palette.accent)
                .frame(width: 12, height: 4)
            Capsule()
                .fill(palette.text.opacity(0.7))
                .frame(width: 18, height: 4)
        }
        .frame(width: 46, height: 28)
        .background(
            RoundedRectangle(cornerRadius: 7, style: .continuous)
                .fill(palette.background)
        )
        .overlay(
            RoundedRectangle(cornerRadius: 7, style: .continuous)
                .stroke(palette.accent.opacity(0.55), lineWidth: 1)
        )
        .accessibilityHidden(true)
    }
}
