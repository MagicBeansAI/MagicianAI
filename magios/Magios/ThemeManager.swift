import SwiftUI
import UIKit

extension Color {
    init(hex: String) {
        let hex = hex.trimmingCharacters(in: CharacterSet.alphanumerics.inverted)
        var int: UInt64 = 0
        Scanner(string: hex).scanHexInt64(&int)
        let a, r, g, b: UInt64
        switch hex.count {
        case 3: // RGB (12-bit)
            (a, r, g, b) = (255, (int >> 8) * 17, (int >> 4 & 0xF) * 17, (int & 0xF) * 17)
        case 6: // RGB (24-bit)
            (a, r, g, b) = (255, int >> 16, int >> 8 & 0xFF, int & 0xFF)
        case 8: // ARGB (32-bit)
            (a, r, g, b) = (int >> 24, int >> 16 & 0xFF, int >> 8 & 0xFF, int & 0xFF)
        default:
            (a, r, g, b) = (255, 0, 0, 0)
        }
        self.init(
            .sRGB,
            red: Double(r) / 255,
            green: Double(g) / 255,
            blue:  Double(b) / 255,
            opacity: Double(a) / 255
        )
    }
}

extension Font {
    /// The active theme's font, read from the shared ThemeManager — use on Text /
    /// Label so any view (regardless of its local theme-variable name) picks up the
    /// per-theme font. Do NOT use on SF Symbol `Image`s: custom fonts have no symbol
    /// glyphs, so icons must keep `.system(...)` / semantic sizes (`.title3`, etc.).
    static func themed(_ size: CGFloat, weight: Font.Weight = .regular) -> Font {
        ThemeManager.shared.font(size, weight: weight, role: .body)
    }

    static func themedDisplay(_ size: CGFloat, weight: Font.Weight = .regular) -> Font {
        ThemeManager.shared.font(size, weight: weight, role: .display)
    }

    static func themedBrand(_ size: CGFloat, weight: Font.Weight = .regular) -> Font {
        ThemeManager.shared.font(size, weight: weight, role: .brand)
    }

    static func themedMono(_ size: CGFloat, weight: Font.Weight = .regular) -> Font {
        ThemeManager.shared.font(size, weight: weight, role: .mono)
    }

    static func themedMono(_ style: Font.TextStyle, weight: Font.Weight = .regular) -> Font {
        ThemeManager.shared.font(style, weight: weight, role: .mono)
    }
}

enum ThemeFontRole {
    case brand, display, body, mono
}

struct ThemeFontNames: Equatable {
    let brand: String
    let display: String
    let body: String
    let mono: String

    func name(for role: ThemeFontRole) -> String {
        switch role {
        case .brand: return brand
        case .display: return display
        case .body: return body
        case .mono: return mono
        }
    }
}

class ThemeManager: ObservableObject {
    struct ThemeFamily: Identifiable, Equatable {
        let name: String
        let lightTheme: String
        let darkTheme: String

        /// The light variant is the stable family identity exposed by Settings.
        /// This keeps the picker free of duplicate light/dark rows while the
        /// persisted `magican-theme` value remains compatible with web theme IDs.
        var id: String { lightTheme }
    }

    /// A resolved variant's colors (used to publish both day + night variants to the
    /// keyboard without disturbing the app's live palette).
    struct ThemePalette {
        let background, surface, elevated, text, secondaryText, accent, accentHover: Color
    }

    /// The app's own day/night mode. `system` follows the iOS appearance.
    enum AppearanceMode: String, CaseIterable, Identifiable {
        case system, day, night
        var id: String { rawValue }
        var label: String {
            switch self {
            case .system: return "System"
            case .day: return "Day"
            case .night: return "Night"
            }
        }
    }

    static let shared = ThemeManager()

    /// Side-effect-free initializer for probing a variant's palette (see
    /// `paletteColors(for:)`). Does NOT apply a theme or touch UIKit appearance.
    private init(probe: Bool) {}

    /// The resolved colors for any theme id — computed on a throwaway instance so it
    /// never disturbs the live app palette.
    static func paletteColors(for name: String) -> ThemePalette {
        let probe = ThemeManager(probe: true)
        probe.applyPalette(name)
        return ThemePalette(
            background: probe.backgroundColor, surface: probe.surfaceColor,
            elevated: probe.elevatedColor, text: probe.textColor,
            secondaryText: probe.secondaryTextColor, accent: probe.accentColor,
            accentHover: probe.accentHoverColor)
    }

    /// Publish BOTH day + night variants of the active family to the keyboard (App
    /// Group), so the keyboard shows the variant matching the SYSTEM appearance —
    /// independent of the app's own mode.
    private func publishKeyboardTheme(for name: String) {
        let family = themeFamily(containing: name) ?? availableThemeFamilies[0]
        let light = Self.paletteColors(for: family.lightTheme)
        let dark = Self.paletteColors(for: family.darkTheme)
        KeyboardThemeStore.save(
            light: KeyboardPalette(
                background: light.background, surface: light.surface, elevated: light.elevated,
                text: light.text, secondaryText: light.secondaryText, accent: light.accent, isDark: false),
            dark: KeyboardPalette(
                background: dark.background, surface: dark.surface, elevated: dark.elevated,
                text: dark.text, secondaryText: dark.secondaryText, accent: dark.accent, isDark: true))
    }
    
    @Published var backgroundColor: Color = Color(hex: "#f3ead6") // longhand base
    @Published var surfaceColor: Color = Color(hex: "#ede1c4")
    @Published var elevatedColor: Color = Color(hex: "#faf3e0")
    @Published var textColor: Color = Color(hex: "#1a1612")
    @Published var secondaryTextColor: Color = Color(hex: "#3a2f24")
    @Published var accentColor: Color = Color(hex: "#a04020")
    @Published var accentHoverColor: Color = Color(hex: "#732c14")
    
    @Published var currentThemeName: String = "longhand"
    /// True when the active theme's background is dark. Drives
    /// `.preferredColorScheme` so SwiftUI's semantic colors (secondary text, nav
    /// titles, Menus, pickers, list defaults) flip to the right light/dark base —
    /// otherwise a dark theme on a light device renders dark-on-dark text.
    @Published var isDark: Bool = false
    /// The app's own day/night mode. `.system` follows the iOS appearance; `.day` /
    /// `.night` force the family's light / dark variant. Persisted separately from the
    /// exact theme id. NOTE: the keyboard ignores this and always follows the system.
    @Published var appearanceMode: AppearanceMode = .system
    /// The active theme's four font roles, matching Web's `--font-brand`,
    /// `--font-display`, `--font-primary`, and `--font-mono` tokens.
    @Published private(set) var fontNames = ThemeFontNames(
        brand: "Outfit", display: "Outfit", body: "Manrope", mono: "Geist Mono"
    )
    var fontName: String { fontNames.body }
    /// Increments only after every value in a theme has been applied. UIKit-backed
    /// toolbar content can cache child views across ordinary `@Published` palette
    /// updates; observing this final revision gives those boundaries one stable,
    /// fully-applied invalidation point.
    @Published private(set) var themeRevision: UInt = 0

    var colorScheme: ColorScheme { isDark ? .dark : .light }

    /// The color scheme to FORCE at the SwiftUI presentation boundary.
    /// In `.system` mode this is `nil` so the app follows the device — critically,
    /// forcing `.preferredColorScheme` here would also force the window trait, which
    /// `systemIsDark` reads back, making `.system` reconciliation see a fake "dark"
    /// device and lock the app to dark. `.day`/`.night` force the resolved scheme.
    var forcedColorScheme: ColorScheme? { appearanceMode == .system ? nil : colorScheme }

    // MARK: - Semantic colors

    /// Reusable application surfaces. Feature views should use these instead of
    /// system grays/whites so cards and controls remain in the selected palette.
    var cardColor: Color { elevatedColor }
    var cardBorderColor: Color { secondaryTextColor.opacity(isDark ? 0.28 : 0.16) }
    var controlColor: Color { surfaceColor }
    var controlBorderColor: Color { secondaryTextColor.opacity(isDark ? 0.34 : 0.20) }

    /// Web's `--bg-soft` token. It is intentionally distinct from the general
    /// surface color: segmented tracks and muted controls otherwise disappear
    /// into their parent surface in several light themes.
    var softBackgroundColor: Color {
        guard let token = Self.themeSoftBackgrounds[currentThemeName] else {
            return surfaceColor
        }
        return Color(hex: token.hex).opacity(token.opacity)
    }

    /// Text/icons placed directly on the active accent. This is an explicit
    /// per-theme contract shared with Web's `--text-on-accent`; deriving it from
    /// luminance made coral themes choose dark text and drift from Web.
    var onAccentColor: Color {
        Color(hex: Self.themeAccentForegrounds[currentThemeName] ?? "#ffffff")
    }

    /// Semantic hues remain recognizable while using light/dark variants chosen
    /// for sufficient contrast against the active application surfaces.
    var successColor: Color { Color(hex: isDark ? "#68d391" : "#16784a") }
    var warningColor: Color { Color(hex: isDark ? "#f6c453" : "#9a5b00") }
    var dangerColor: Color { Color(hex: isDark ? "#ff7b7b" : "#b4232f") }
    var infoColor: Color { Color(hex: isDark ? "#79b8ff" : "#1769aa") }
    var discoveryColor: Color { Color(hex: isDark ? "#d3a6ff" : "#7040a0") }

    /// Foreground for a solid semantic/action fill. Keep this centralized so
    /// swipe actions and feature-specific buttons do not assume white text.
    func contrastingTextColor(for color: Color) -> Color {
        let light = Color.white
        let dark = Color(hex: "#111111")
        let backgroundLuminance = Self.relativeLuminance(of: color)
        let lightContrast = 1.05 / (backgroundLuminance + 0.05)
        let darkContrast = (backgroundLuminance + 0.05) / (Self.relativeLuminance(of: dark) + 0.05)
        return darkContrast > lightContrast ? dark : light
    }

    /// Exact native mirror of Web's four-role font table for all 22 variants.
    static let themeFonts: [String: ThemeFontNames] = [
        "longhand": .init(brand: "Outfit", display: "Outfit", body: "Manrope", mono: "Geist Mono"),
        "longhand-dark": .init(brand: "Outfit", display: "Outfit", body: "Manrope", mono: "Geist Mono"),
        "soft-machine": .init(brand: "Outfit", display: "Space Grotesk", body: "Quicksand", mono: "JetBrains Mono"),
        "soft-machine-dark": .init(brand: "Outfit", display: "Fredoka", body: "Quicksand", mono: "JetBrains Mono"),
        "arcane-terminal": .init(brand: "Outfit", display: "Fira Code", body: "IBM Plex Mono", mono: "Fira Code"),
        "arcane-terminal-light": .init(brand: "Outfit", display: "Fira Code", body: "IBM Plex Mono", mono: "Fira Code"),
        "retro-16bit": .init(brand: "Outfit", display: "IBM Plex Mono", body: "JetBrains Mono", mono: "IBM Plex Mono"),
        "retro-16bit-light": .init(brand: "Outfit", display: "IBM Plex Mono", body: "JetBrains Mono", mono: "JetBrains Mono"),
        "mario-8bit": .init(brand: "Outfit", display: "Press Start 2P", body: "Pixelify Sans", mono: "Press Start 2P"),
        "mario-8bit-dark": .init(brand: "Outfit", display: "Press Start 2P", body: "Pixelify Sans", mono: "Press Start 2P"),
        "risograph": .init(brand: "Outfit", display: "Bricolage Grotesque", body: "Manrope", mono: "JetBrains Mono"),
        "risograph-dark": .init(brand: "Outfit", display: "Bricolage Grotesque", body: "Manrope", mono: "JetBrains Mono"),
        "mixtape": .init(brand: "Outfit", display: "Permanent Marker", body: "Special Elite", mono: "IBM Plex Mono"),
        "mixtape-dark": .init(brand: "Outfit", display: "Permanent Marker", body: "Special Elite", mono: "IBM Plex Mono"),
        "mono": .init(brand: "Outfit", display: "Space Grotesk", body: "Inter", mono: "JetBrains Mono"),
        "mono-dark": .init(brand: "Outfit", display: "Space Grotesk", body: "Inter", mono: "JetBrains Mono"),
        "cartoon": .init(brand: "Outfit", display: "Lilita One", body: "Fredoka", mono: "JetBrains Mono"),
        "cartoon-dark": .init(brand: "Outfit", display: "Lilita One", body: "Fredoka", mono: "JetBrains Mono"),
        "bubbly": .init(brand: "Outfit", display: "Fredoka", body: "Quicksand", mono: "JetBrains Mono"),
        "bubbly-dark": .init(brand: "Outfit", display: "Fredoka", body: "Quicksand", mono: "JetBrains Mono"),
        "jarvis": .init(brand: "Outfit", display: "Rajdhani", body: "Manrope", mono: "JetBrains Mono"),
        "jarvis-light": .init(brand: "Outfit", display: "Rajdhani", body: "Manrope", mono: "JetBrains Mono")
    ]

    static func fonts(for theme: String) -> ThemeFontNames {
        themeFonts[theme] ?? themeFonts["longhand"]!
    }

    /// Mirrors each Web theme's `--text-on-accent` value. Keep explicit values
    /// for both variants so adding a theme cannot silently inherit the wrong
    /// foreground from another family.
    private static let themeAccentForegrounds: [String: String] = [
        "longhand": "#faf3e0", "longhand-dark": "#1a1612",
        "soft-machine": "#ffffff", "soft-machine-dark": "#ffffff",
        "arcane-terminal": "#ffffff", "arcane-terminal-light": "#f6f8fa",
        "retro-16bit": "#ffffff", "retro-16bit-light": "#ffffff",
        "mario-8bit": "#ffffff", "mario-8bit-dark": "#000000",
        "risograph": "#fbf6e8", "risograph-dark": "#14110d",
        "mixtape": "#f4e4b3", "mixtape-dark": "#0f0e0c",
        "mono": "#ffffff", "mono-dark": "#000000",
        "cartoon": "#0a0a08", "cartoon-dark": "#1a1830",
        "bubbly": "#ffffff", "bubbly-dark": "#171b1d",
        "jarvis": "#03121f", "jarvis-light": "#ffffff"
    ]

    /// Mirrors Web's per-theme `--bg-soft`, including translucent themes where
    /// the token is an rgba overlay rather than an opaque surface.
    private static let themeSoftBackgrounds: [String: (hex: String, opacity: Double)] = [
        "longhand": ("#e3d3ac", 1), "longhand-dark": ("#322a22", 1),
        "soft-machine": ("#f3f0ea", 1), "soft-machine-dark": ("#20272a", 1),
        "arcane-terminal": ("#1a1a2e", 1), "arcane-terminal-light": ("#e2e7ec", 1),
        "retro-16bit": ("#14110d", 1), "retro-16bit-light": ("#e0e0db", 1),
        "mario-8bit": ("#000000", 0.08), "mario-8bit-dark": ("#ffffff", 0.08),
        "risograph": ("#1a1612", 0.06), "risograph-dark": ("#f4efe1", 0.06),
        "mixtape": ("#2a1a0e", 0.06), "mixtape-dark": ("#e8c66a", 0.08),
        "mono": ("#000000", 0.05), "mono-dark": ("#ffffff", 0.04),
        "cartoon": ("#0a0a08", 0.06), "cartoon-dark": ("#fff5e1", 0.06),
        "bubbly": ("#f7f3eb", 1), "bubbly-dark": ("#20272a", 1),
        "jarvis": ("#0f1a2e", 1), "jarvis-light": ("#dceaf5", 1)
    ]

    /// Resolve a role through the active theme. `Font.custom` safely falls back
    /// when a family cannot be registered.
    func font(
        _ size: CGFloat,
        weight: Font.Weight = .regular,
        role: ThemeFontRole = .body
    ) -> Font {
        Font.custom(fontNames.name(for: role), size: size).weight(weight)
    }

    func font(
        _ style: Font.TextStyle,
        weight: Font.Weight = .regular,
        role: ThemeFontRole = .body
    ) -> Font {
        Font.custom(fontNames.name(for: role), size: Self.pointSize(for: style), relativeTo: style)
            .weight(weight)
    }

    private static func pointSize(for style: Font.TextStyle) -> CGFloat {
        switch style {
        case .largeTitle: return 34
        case .title: return 28
        case .title2: return 22
        case .title3: return 20
        case .headline, .body: return 17
        case .callout: return 16
        case .subheadline: return 15
        case .footnote: return 13
        case .caption: return 12
        case .caption2: return 11
        default: return 17
        }
    }

    /// UIFont for the active theme (for UIKit appearance proxies) — family name,
    /// else system.
    private func uiFont(
        _ size: CGFloat,
        weight: UIFont.Weight = .regular,
        role: ThemeFontRole = .body
    ) -> UIFont {
        UIFont(name: fontNames.name(for: role), size: size)
            ?? .systemFont(ofSize: size, weight: weight)
    }

    /// Theme UIKit-owned navigation/tab labels and icons. SwiftUI observes the
    /// palette directly, but `NavigationView` and `TabView` cache their UIKit
    /// appearances and otherwise keep stale text colors until a cold launch.
    private func applySystemAppearance() {
        let navigation = UINavigationBarAppearance()
        navigation.configureWithOpaqueBackground()
        navigation.backgroundColor = UIColor(backgroundColor)
        navigation.shadowColor = .clear
        navigation.titleTextAttributes = [
            .font: uiFont(17, weight: .semibold, role: .display),
            .foregroundColor: UIColor(textColor)
        ]
        navigation.largeTitleTextAttributes = [
            .font: uiFont(30, weight: .bold, role: .display),
            .foregroundColor: UIColor(textColor)
        ]
        UINavigationBar.appearance().standardAppearance = navigation
        UINavigationBar.appearance().scrollEdgeAppearance = navigation
        UINavigationBar.appearance().compactAppearance = navigation

        let accent = UIColor(accentColor)
        let secondary = UIColor(secondaryTextColor)
        UINavigationBar.appearance().tintColor = accent

        let tab = UITabBarAppearance()
        tab.configureWithOpaqueBackground()
        tab.backgroundColor = UIColor(backgroundColor)
        tab.shadowColor = .clear
        for item in [
            tab.stackedLayoutAppearance,
            tab.inlineLayoutAppearance,
            tab.compactInlineLayoutAppearance
        ] {
            item.normal.iconColor = secondary
            item.normal.titleTextAttributes = [
                .font: uiFont(10, weight: .medium, role: .display),
                .foregroundColor: secondary
            ]
            item.selected.iconColor = accent
            item.selected.titleTextAttributes = [
                .font: uiFont(10, weight: .semibold, role: .display),
                .foregroundColor: accent
            ]
        }
        UITabBar.appearance().standardAppearance = tab
        UITabBar.appearance().scrollEdgeAppearance = tab
        UITabBar.appearance().tintColor = accent
        UITabBar.appearance().unselectedItemTintColor = secondary

        // Appearance proxies only affect newly created UIKit controls. Push the
        // same values into controls already hosted by SwiftUI for a live switch.
        for scene in UIApplication.shared.connectedScenes.compactMap({ $0 as? UIWindowScene }) {
            for window in scene.windows {
                // Keep UIKit's trait collection in lockstep with SwiftUI. This is
                // especially important for navigation-hosted labels and sheets,
                // which otherwise retain semantic colors until a relaunch. In
                // `.system` mode leave the window unspecified so it follows the device.
                window.overrideUserInterfaceStyle = appearanceMode == .system ? .unspecified : (isDark ? .dark : .light)
                window.tintColor = accent
                Self.forEachNavigationBar(in: window) { bar in
                    bar.standardAppearance = navigation
                    bar.scrollEdgeAppearance = navigation
                    bar.compactAppearance = navigation
                    bar.tintColor = accent
                    bar.barStyle = isDark ? .black : .default
                    bar.setNeedsLayout()
                }
                Self.forEachTabBar(in: window) { bar in
                    bar.standardAppearance = tab
                    bar.scrollEdgeAppearance = tab
                    bar.tintColor = accent
                    bar.unselectedItemTintColor = secondary
                    bar.setNeedsLayout()
                }
            }
        }
    }

    /// Recursively visit every `UINavigationBar` in a view tree.
    private static func forEachNavigationBar(in view: UIView, _ apply: (UINavigationBar) -> Void) {
        if let bar = view as? UINavigationBar { apply(bar) }
        for sub in view.subviews { Self.forEachNavigationBar(in: sub, apply) }
    }

    /// Recursively visit every `UITabBar` in a view tree.
    private static func forEachTabBar(in view: UIView, _ apply: (UITabBar) -> Void) {
        if let bar = view as? UITabBar { apply(bar) }
        for sub in view.subviews { Self.forEachTabBar(in: sub, apply) }
    }

    let availableThemeFamilies: [ThemeFamily] = [
        ThemeFamily(name: "Longhand", lightTheme: "longhand", darkTheme: "longhand-dark"),
        ThemeFamily(name: "Soft Machine", lightTheme: "soft-machine", darkTheme: "soft-machine-dark"),
        ThemeFamily(name: "Arcane Terminal", lightTheme: "arcane-terminal-light", darkTheme: "arcane-terminal"),
        ThemeFamily(name: "Retro 16-bit", lightTheme: "retro-16bit-light", darkTheme: "retro-16bit"),
        ThemeFamily(name: "Mario 8-bit", lightTheme: "mario-8bit", darkTheme: "mario-8bit-dark"),
        ThemeFamily(name: "Risograph", lightTheme: "risograph", darkTheme: "risograph-dark"),
        ThemeFamily(name: "Mixtape", lightTheme: "mixtape", darkTheme: "mixtape-dark"),
        ThemeFamily(name: "Mono", lightTheme: "mono", darkTheme: "mono-dark"),
        ThemeFamily(name: "2D Cartoon", lightTheme: "cartoon", darkTheme: "cartoon-dark"),
        ThemeFamily(name: "Bubbly", lightTheme: "bubbly", darkTheme: "bubbly-dark"),
        ThemeFamily(name: "Jarvis", lightTheme: "jarvis-light", darkTheme: "jarvis")
    ]

    /// Full variant catalog retained for compatibility with persisted theme IDs
    /// and code that needs to inspect both palettes. Settings intentionally uses
    /// `availableThemeFamilies` instead.
    var availableThemes: [String] {
        availableThemeFamilies.flatMap { [$0.lightTheme, $0.darkTheme] }
    }

    /// Picker selection for the active family, represented by its light variant
    /// even while the dark companion is active.
    var currentThemeFamilyID: String {
        themeFamily(containing: currentThemeName)?.id ?? availableThemeFamilies[0].id
    }
    
    init() {
        if let raw = UserDefaults.standard.string(forKey: "magican-appearance-mode"),
           let mode = AppearanceMode(rawValue: raw) {
            appearanceMode = mode
        }
        if let saved = UserDefaults.standard.string(forKey: "magican-theme") {
            applyTheme(saved)
        } else {
            applyTheme("longhand")
        }
    }
    
    func applyTheme(_ name: String) {
        applyPalette(name)
        fontNames = Self.fonts(for: name)
        isDark = Self.luminance(of: backgroundColor) < 0.5
        currentThemeName = name
        UserDefaults.standard.set(name, forKey: "magican-theme")
        themeRevision &+= 1
        applySystemAppearance()

        // Share BOTH variants of the active family with the Magican keyboard extension
        // (App Group) so it can pick day/night by the SYSTEM appearance.
        publishKeyboardTheme(for: name)

        // `.preferredColorScheme` changes UIKit traits after this synchronous
        // setter returns. Re-apply on the next main-loop turn so that transition
        // cannot overwrite our navigation/tab text colors with the old scheme.
        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            self.applySystemAppearance()
            // Also invalidate singleton readers after all palette fields and the
            // UIKit trait transition have settled.
            self.objectWillChange.send()
        }
    }

    /// Select a theme family, applying the variant that matches the current
    /// appearance mode (+ the system appearance when the mode is `.system`).
    func applyThemeFamily(_ familyID: String, systemDark: Bool) {
        guard let family = availableThemeFamilies.first(where: { $0.id == familyID }) else { return }
        applyTheme(resolvedDark(systemDark: systemDark) ? family.darkTheme : family.lightTheme)
    }

    /// The variant (dark?) the app should show given the mode + system appearance.
    func resolvedDark(systemDark: Bool) -> Bool {
        switch appearanceMode {
        case .day: return false
        case .night: return true
        case .system: return systemDark
        }
    }

    /// The colors Settings should preview for a family right now.
    ///
    /// This resolves through the same appearance rule as `applyThemeFamily`, so
    /// the picker cannot promise a light palette and then apply its dark peer.
    /// The probe is side-effect free and does not disturb the active theme.
    func previewPalette(for family: ThemeFamily, systemDark: Bool) -> ThemePalette {
        Self.paletteColors(
            for: resolvedDark(systemDark: systemDark) ? family.darkTheme : family.lightTheme
        )
    }

    /// Set the app's day/night mode and re-apply the active family's matching variant.
    func setAppearanceMode(_ mode: AppearanceMode, systemDark: Bool) {
        appearanceMode = mode
        UserDefaults.standard.set(mode.rawValue, forKey: "magican-appearance-mode")
        guard let family = themeFamily(containing: currentThemeName) else { return }
        applyTheme(resolvedDark(systemDark: systemDark) ? family.darkTheme : family.lightTheme)
    }

    /// Called by the app root when the iOS appearance flips — re-applies the family's
    /// variant only when the app is following the system.
    func systemAppearanceChanged(dark: Bool) {
        guard appearanceMode == .system, let family = themeFamily(containing: currentThemeName) else { return }
        let want = dark ? family.darkTheme : family.lightTheme
        if want != currentThemeName { applyTheme(want) }
    }

    /// Switch the active family between its paired light and dark palettes.
    /// Exact theme IDs remain persisted, matching the web theme contract.
    func setDarkMode(_ enabled: Bool) {
        guard let family = themeFamily(containing: currentThemeName) else { return }
        applyTheme(enabled ? family.darkTheme : family.lightTheme)
    }

    private func themeFamily(containing theme: String) -> ThemeFamily? {
        availableThemeFamilies.first {
            $0.lightTheme == theme || $0.darkTheme == theme
        }
    }

    /// The true device (system) appearance, ignoring the app's own window override —
    /// read from the window scene, whose trait reflects the system. Used to resolve
    /// the `.system` mode.
    static var systemIsDark: Bool {
        UIApplication.shared.connectedScenes
            .compactMap { $0 as? UIWindowScene }
            .first?.traitCollection.userInterfaceStyle == .dark
    }

    /// Relative luminance (0…1) of a color — used to decide if a theme is dark.
    private static func luminance(of color: Color) -> Double {
        var r: CGFloat = 0, g: CGFloat = 0, b: CGFloat = 0, a: CGFloat = 0
        UIColor(color).getRed(&r, green: &g, blue: &b, alpha: &a)
        return 0.299 * Double(r) + 0.587 * Double(g) + 0.114 * Double(b)
    }

    /// WCAG relative luminance used when selecting text for a solid fill.
    private static func relativeLuminance(of color: Color) -> Double {
        var red: CGFloat = 0
        var green: CGFloat = 0
        var blue: CGFloat = 0
        var alpha: CGFloat = 0
        UIColor(color).getRed(&red, green: &green, blue: &blue, alpha: &alpha)
        func linear(_ component: CGFloat) -> Double {
            let value = Double(component)
            return value <= 0.04045 ? value / 12.92 : pow((value + 0.055) / 1.055, 2.4)
        }
        return 0.2126 * linear(red) + 0.7152 * linear(green) + 0.0722 * linear(blue)
    }

    private func applyPalette(_ name: String) {
        switch name {
        case "soft-machine":
            backgroundColor = Color(hex: "#fefdfb")
            surfaceColor = Color(hex: "#f8f6f2")
            elevatedColor = Color(hex: "#ffffff")
            textColor = Color(hex: "#2d2a26")
            secondaryTextColor = Color(hex: "#4a4540")
            accentColor = Color(hex: "#e85d5d")
            accentHoverColor = Color(hex: "#d04f4f")
        case "soft-machine-dark":
            backgroundColor = Color(hex: "#171b1d")
            surfaceColor = Color(hex: "#1d2326")
            elevatedColor = Color(hex: "#242b2f")
            textColor = Color(hex: "#f7f1e8")
            secondaryTextColor = Color(hex: "#d7cfc2")
            accentColor = Color(hex: "#ff7b7b")
            accentHoverColor = Color(hex: "#ff9393")
        case "arcane-terminal":
            backgroundColor = Color(hex: "#0a0a0f")
            surfaceColor = Color(hex: "#12121a")
            elevatedColor = Color(hex: "#1a1a2e")
            textColor = Color(hex: "#e0e0e0")
            secondaryTextColor = Color(hex: "#b0b0b0")
            accentColor = Color(hex: "#00d4aa")
            accentHoverColor = Color(hex: "#00f5c4")
        case "arcane-terminal-light":
            backgroundColor = Color(hex: "#f6f8fa")
            surfaceColor = Color(hex: "#eef1f4")
            elevatedColor = Color(hex: "#ffffff")
            textColor = Color(hex: "#0a0a0f")
            secondaryTextColor = Color(hex: "#2a2a35")
            accentColor = Color(hex: "#007a66")
            accentHoverColor = Color(hex: "#00604f")
        case "retro-16bit":
            backgroundColor = Color(hex: "#0c0a08")
            surfaceColor = Color(hex: "#14110d")
            elevatedColor = Color(hex: "#1a1610")
            textColor = Color(hex: "#ffb000")
            secondaryTextColor = Color(hex: "#ffcc00")
            accentColor = Color(hex: "#ffb000")
            accentHoverColor = Color(hex: "#ffd000")
        case "retro-16bit-light":
            backgroundColor = Color(hex: "#f5f5f0")
            surfaceColor = Color(hex: "#ebebe6")
            elevatedColor = Color(hex: "#ffffff")
            textColor = Color(hex: "#1a1a1a")
            secondaryTextColor = Color(hex: "#333333")
            accentColor = Color(hex: "#1a1a1a")
            accentHoverColor = Color(hex: "#000000")
        case "mario-8bit":
            backgroundColor = Color(hex: "#5c94fc")
            surfaceColor = Color(hex: "#88b8fc")
            elevatedColor = Color(hex: "#ffffff")
            textColor = Color(hex: "#000000")
            secondaryTextColor = Color(hex: "#1a1a1a")
            accentColor = Color(hex: "#e40000")
            accentHoverColor = Color(hex: "#b80000")
        case "mario-8bit-dark":
            backgroundColor = Color(hex: "#000000")
            surfaceColor = Color(hex: "#181818")
            elevatedColor = Color(hex: "#2a2a2a")
            textColor = Color(hex: "#ffffff")
            secondaryTextColor = Color(hex: "#e0e0e0")
            accentColor = Color(hex: "#fbd000")
            accentHoverColor = Color(hex: "#ffe040")
        case "risograph":
            backgroundColor = Color(hex: "#f5efe1")
            surfaceColor = Color(hex: "#ede5d2")
            elevatedColor = Color(hex: "#fbf6e8")
            textColor = Color(hex: "#1a1612")
            secondaryTextColor = Color(hex: "#2d2820")
            accentColor = Color(hex: "#ff48b0")
            accentHoverColor = Color(hex: "#e02e90")
        case "risograph-dark":
            backgroundColor = Color(hex: "#14110d")
            surfaceColor = Color(hex: "#1f1a14")
            elevatedColor = Color(hex: "#2a2218")
            textColor = Color(hex: "#f4efe1")
            secondaryTextColor = Color(hex: "#d8d2c1")
            accentColor = Color(hex: "#ff48b0")
            accentHoverColor = Color(hex: "#ff70c4")
        case "mixtape":
            backgroundColor = Color(hex: "#e9d59f")
            surfaceColor = Color(hex: "#dcc692")
            elevatedColor = Color(hex: "#f4e4b3")
            textColor = Color(hex: "#2a1a0e")
            secondaryTextColor = Color(hex: "#3e2a18")
            accentColor = Color(hex: "#c8362a")
            accentHoverColor = Color(hex: "#a82418")
        case "mixtape-dark":
            backgroundColor = Color(hex: "#0f0e0c")
            surfaceColor = Color(hex: "#1a1815")
            elevatedColor = Color(hex: "#2a2520")
            textColor = Color(hex: "#e8c66a")
            secondaryTextColor = Color(hex: "#d8b358")
            accentColor = Color(hex: "#ff5040")
            accentHoverColor = Color(hex: "#ff6e60")
        case "mono":
            backgroundColor = Color(hex: "#f4f4f5")
            surfaceColor = Color(hex: "#e4e4e7")
            elevatedColor = Color(hex: "#ffffff")
            textColor = Color(hex: "#000000")
            secondaryTextColor = Color(hex: "#1a1a1a")
            accentColor = Color(hex: "#000000")
            accentHoverColor = Color(hex: "#1a1a1a")
        case "mono-dark":
            backgroundColor = Color(hex: "#000000")
            surfaceColor = Color(hex: "#0a0a0a")
            elevatedColor = Color(hex: "#141414")
            textColor = Color(hex: "#ffffff")
            secondaryTextColor = Color(hex: "#e5e5e5")
            accentColor = Color(hex: "#ffffff")
            accentHoverColor = Color(hex: "#e5e5e5")
        case "cartoon":
            backgroundColor = Color(hex: "#9dd5ff")
            surfaceColor = Color(hex: "#84c8ff")
            elevatedColor = Color(hex: "#ffffff")
            textColor = Color(hex: "#0a0a08")
            secondaryTextColor = Color(hex: "#1a1a14")
            accentColor = Color(hex: "#ff5499")
            accentHoverColor = Color(hex: "#ff3886")
        case "cartoon-dark":
            backgroundColor = Color(hex: "#1a1830")
            surfaceColor = Color(hex: "#25224a")
            elevatedColor = Color(hex: "#2c2956")
            textColor = Color(hex: "#fff5e1")
            secondaryTextColor = Color(hex: "#ede4ce")
            accentColor = Color(hex: "#ff7eb6")
            accentHoverColor = Color(hex: "#ff9ec8")
        case "bubbly":
            backgroundColor = Color(hex: "#fdfcf8")
            surfaceColor = Color(hex: "#fff8f2")
            elevatedColor = Color(hex: "#ffffff")
            textColor = Color(hex: "#2d3436")
            secondaryTextColor = Color(hex: "#5f6668")
            accentColor = Color(hex: "#ff6b6b")
            accentHoverColor = Color(hex: "#ff5252")
        case "bubbly-dark":
            backgroundColor = Color(hex: "#171b1d")
            surfaceColor = Color(hex: "#1d2326")
            elevatedColor = Color(hex: "#252c30")
            textColor = Color(hex: "#f7f1e8")
            secondaryTextColor = Color(hex: "#d8cfc2")
            accentColor = Color(hex: "#ff7b7b")
            accentHoverColor = Color(hex: "#ff9a9a")
        case "longhand-dark":
            backgroundColor = Color(hex: "#1a1612")
            surfaceColor = Color(hex: "#221d18")
            elevatedColor = Color(hex: "#2a241e")
            textColor = Color(hex: "#f3ead6")
            secondaryTextColor = Color(hex: "#d6c8a8")
            accentColor = Color(hex: "#d8602e")
            accentHoverColor = Color(hex: "#e87a4a")
        case "jarvis":
            backgroundColor = Color(hex: "#050a14")
            surfaceColor = Color(hex: "#0a1220")
            elevatedColor = Color(hex: "#0e1828")
            textColor = Color(hex: "#d8ecff")
            secondaryTextColor = Color(hex: "#98b6d6")
            accentColor = Color(hex: "#00d4ff")
            accentHoverColor = Color(hex: "#33dfff")
        case "jarvis-light":
            backgroundColor = Color(hex: "#f0f7ff")
            surfaceColor = Color(hex: "#e6f1fa")
            elevatedColor = Color(hex: "#ffffff")
            textColor = Color(hex: "#062035")
            secondaryTextColor = Color(hex: "#2d4a6b")
            accentColor = Color(hex: "#0099cc")
            accentHoverColor = Color(hex: "#00b3e8")
        case "longhand":
            fallthrough
        default:
            backgroundColor = Color(hex: "#f3ead6")
            surfaceColor = Color(hex: "#ede1c4")
            elevatedColor = Color(hex: "#faf3e0")
            textColor = Color(hex: "#1a1612")
            secondaryTextColor = Color(hex: "#3a2f24")
            accentColor = Color(hex: "#a04020")
            accentHoverColor = Color(hex: "#732c14")
        }
    }
}
