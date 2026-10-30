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
        var accentFill: Color? = nil
    }

    /// A variant's palette as Web hex tokens — the inspectable source every
    /// applied palette reads. Field ↔ Web token: `background` `--bg-base`,
    /// `surface` `--bg-surface`, `elevated` `--bg-elevated`, `text`
    /// `--text-primary`, `secondaryText` `--text-secondary`, `accent`
    /// `--accent-primary`, `accentHover` `--accent-primary-hover`,
    /// `secondaryAccent` `--accent-secondary`, `wash` the `--wash-hero`
    /// stops (three tints, then the base), and `accentFill` /
    /// `accentFillPressed` `--accent-fill` / `--accent-fill-pressed`. Themes
    /// whose Web block predates the optional tokens leave them `nil`.
    struct PaletteTokens: Equatable {
        let background, surface, elevated, text, secondaryText, accent, accentHover: String
        /// Web's `--accent-fill` / `--accent-fill-pressed`: the solid fill
        /// behind on-accent text. Dark variants set a deeper shade of their
        /// accent (same HSL hue and saturation, lightness lowered until white
        /// reaches 4.5:1; pressed about 5.5:1). Light variants leave them
        /// `nil` and fill with `accent` / `accentHover`.
        var accentFill: String? = nil
        var accentFillPressed: String? = nil
        var secondaryAccent: String? = nil
        var wash: [String]? = nil

        /// The resolved fill hex: the explicit token or the accent itself.
        var resolvedAccentFill: String { accentFill ?? accent }
        /// The resolved pressed-fill hex: the explicit token or `accentHover`.
        var resolvedAccentFillPressed: String { accentFillPressed ?? accentHover }
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

    /// The theme a reader with no saved choice gets. Appearance stays System,
    /// so a dark device resolves it to `sorbet-dark` through the family.
    static let defaultThemeID = "sorbet"

    /// Retired theme ids and the Sorbet variant each now resolves to (the
    /// day/night side is kept). Bubbly and Soft Machine were removed on iOS.
    private static let retiredThemeIDs: [String: String] = [
        "bubbly": "sorbet", "bubbly-dark": "sorbet-dark",
        "soft-machine": "sorbet", "soft-machine-dark": "sorbet-dark"
    ]

    /// The current id for a possibly retired one; live ids pass through.
    static func migratedThemeID(_ id: String) -> String {
        retiredThemeIDs[id] ?? id
    }

    /// Where the theme id and appearance mode persist (`.standard` in the app;
    /// tests inject a private suite).
    private let defaults: UserDefaults

    /// Side-effect-free initializer for probing a variant's palette (see
    /// `paletteColors(for:)`). Does NOT apply a theme or touch UIKit appearance.
    private init(probe: Bool) { defaults = .standard }

    /// The resolved colors for any theme id — computed on a throwaway instance so it
    /// never disturbs the live app palette.
    static func paletteColors(for name: String) -> ThemePalette {
        let probe = ThemeManager(probe: true)
        probe.applyPalette(name)
        return ThemePalette(
            background: probe.backgroundColor, surface: probe.surfaceColor,
            elevated: probe.elevatedColor, text: probe.textColor,
            secondaryText: probe.secondaryTextColor, accent: probe.accentColor,
            accentHover: probe.accentHoverColor, accentFill: probe.accentFillColor)
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
                text: light.text, secondaryText: light.secondaryText, accent: light.accent, isDark: false,
                accentFill: light.accentFill),
            dark: KeyboardPalette(
                background: dark.background, surface: dark.surface, elevated: dark.elevated,
                text: dark.text, secondaryText: dark.secondaryText, accent: dark.accent, isDark: true,
                accentFill: dark.accentFill))
    }
    
    @Published var backgroundColor: Color = Color(hex: "#f3ead6") // longhand base
    @Published var surfaceColor: Color = Color(hex: "#ede1c4")
    @Published var elevatedColor: Color = Color(hex: "#faf3e0")
    @Published var textColor: Color = Color(hex: "#1a1612")
    @Published var secondaryTextColor: Color = Color(hex: "#3a2f24")
    @Published var accentColor: Color = Color(hex: "#a04020")
    @Published var accentHoverColor: Color = Color(hex: "#732c14")
    /// Solid fill behind `onAccentColor` content: primary buttons, the user's
    /// chat bubble, selected chips/segments, badges, the send button. Equals
    /// `accentColor` in light themes; a deeper shade in dark themes so white
    /// on-accent text reads at 4.5:1 or better. Accent-tinted text, strokes,
    /// outlines, progress and soft tints keep the bright `accentColor`.
    @Published var accentFillColor: Color = Color(hex: "#a04020")
    /// Pressed state of `accentFillColor` (`accentHoverColor` in light themes).
    @Published var accentFillPressedColor: Color = Color(hex: "#732c14")
    /// Web's `--accent-secondary`. Themes without one use `discoveryColor`.
    @Published var secondaryAccentColor: Color = Color(hex: "#7040a0")
    
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

    /// Account sync hook: invoked after an EXPLICIT theme application with the
    /// resolved variant and mode, so the app can push the choice to
    /// `/ui/preferences` (see `ThemeSync`). An optional injection point rather
    /// than a direct call so probes, previews and unit tests apply palettes
    /// without any networking. System-driven flips (`.system` following the
    /// device) deliberately do not fire it — the account records choices, not
    /// each device's day/night.
    var themeSyncSink: ((String, AppearanceMode) -> Void)?

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

    /// Text/icons placed directly on the active accent FILL (`accentFillColor`,
    /// never the bright `accentColor`). This is an explicit per-theme contract
    /// shared with Web's `--text-on-accent`; deriving it from luminance made
    /// coral themes choose dark text and drift from Web. Every dark variant
    /// uses white, carried by its deeper `accentFill`.
    var onAccentColor: Color {
        Color(hex: Self.themeAccentForegrounds[currentThemeName] ?? "#ffffff")
    }

    /// Semantic hues remain recognizable while using light/dark variants chosen
    /// for sufficient contrast against the active application surfaces.
    var successColor: Color { Color(hex: isDark ? "#68d391" : "#16784a") }
    var warningColor: Color { Color(hex: isDark ? "#f6c453" : "#9a5b00") }
    var dangerColor: Color { Color(hex: isDark ? "#ff7b7b" : "#b4232f") }
    var infoColor: Color { Color(hex: isDark ? "#79b8ff" : "#1769aa") }

    /// A running task's colour: the web's `--status-running`, which is each
    /// theme's `--color-info` (Sorbet's blue, not its coral accent). The live
    /// task card's breathing dot uses it.
    var statusRunningColor: Color {
        Self.themeStatusRunning[currentThemeName].map { Color(hex: $0) } ?? infoColor
    }

    /// Mirrors each web theme's `--color-info` (= `--status-running`).
    static let themeStatusRunning: [String: String] = [
        "sorbet": "#4a78e0", "sorbet-dark": "#8fb0ff",
        "sky": "#3d7fe0", "sky-dark": "#8cc4ff",
        "candy": "#4a78e0", "candy-dark": "#8fb0ff",
        "longhand": "#4a6a8a", "longhand-dark": "#6db1bf",
        "arcane-terminal": "#00d4ff", "arcane-terminal-light": "#0891b2",
        "retro-16bit": "#ffb000", "retro-16bit-light": "#1a1a1a",
        "mario-8bit": "#3399ff", "mario-8bit-dark": "#5599ff",
        "risograph": "#3d5588", "risograph-dark": "#6ba6d8",
        "mixtape": "#3d6e8a", "mixtape-dark": "#6da3c8",
        "mono": "#4a4a4a", "mono-dark": "#cccccc",
        "cartoon": "#4ec0ff", "cartoon-dark": "#8fdcff",
        "jarvis": "#00d4ff", "jarvis-light": "#0099cc"
    ]
    var discoveryColor: Color { Color(hex: isDark ? "#d3a6ff" : "#7040a0") }

    /// The solid fill for a caller-supplied tint: the active accent maps to
    /// `accentFillColor` (deeper in dark themes so white reads on it); any
    /// other tint fills as itself.
    func solidFill(for tint: Color) -> Color {
        tint == accentColor ? accentFillColor : tint
    }

    /// Pair accent actions with the theme's explicit foreground; semantic
    /// fills choose contrasting text independently of the accent palette.
    func solidForeground(for tint: Color) -> Color {
        tint == accentColor ? onAccentColor : contrastingTextColor(for: tint)
    }

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

    /// Exact native mirror of Web's four-role font table for every variant.
    static let themeFonts: [String: ThemeFontNames] = [
        "sorbet": .init(brand: "Outfit", display: "Fredoka", body: "Nunito", mono: "Geist Mono"),
        "sorbet-dark": .init(brand: "Outfit", display: "Fredoka", body: "Nunito", mono: "Geist Mono"),
        "sky": .init(brand: "Outfit", display: "Outfit", body: "Nunito", mono: "Geist Mono"),
        "sky-dark": .init(brand: "Outfit", display: "Outfit", body: "Nunito", mono: "Geist Mono"),
        "candy": .init(brand: "Outfit", display: "Baloo 2", body: "Nunito", mono: "Geist Mono"),
        "candy-dark": .init(brand: "Outfit", display: "Baloo 2", body: "Nunito", mono: "Geist Mono"),
        "longhand": .init(brand: "Outfit", display: "Outfit", body: "Manrope", mono: "Geist Mono"),
        "longhand-dark": .init(brand: "Outfit", display: "Outfit", body: "Manrope", mono: "Geist Mono"),
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
        "sorbet": "#ffffff", "sorbet-dark": "#ffffff",
        "sky": "#ffffff", "sky-dark": "#ffffff",
        "candy": "#ffffff", "candy-dark": "#ffffff",
        "longhand": "#faf3e0", "longhand-dark": "#ffffff",
        "arcane-terminal": "#ffffff", "arcane-terminal-light": "#f6f8fa",
        "retro-16bit": "#ffffff", "retro-16bit-light": "#ffffff",
        "mario-8bit": "#ffffff", "mario-8bit-dark": "#ffffff",
        "risograph": "#fbf6e8", "risograph-dark": "#ffffff",
        "mixtape": "#f4e4b3", "mixtape-dark": "#ffffff",
        "mono": "#ffffff", "mono-dark": "#ffffff",
        "cartoon": "#0a0a08", "cartoon-dark": "#ffffff",
        "jarvis": "#ffffff", "jarvis-light": "#ffffff"
    ]

    /// Mirrors Web's per-theme `--bg-soft`, including translucent themes where
    /// the token is an rgba overlay rather than an opaque surface.
    private static let themeSoftBackgrounds: [String: (hex: String, opacity: Double)] = [
        "sorbet": ("#fbeee6", 1), "sorbet-dark": ("#2f2436", 1),
        "sky": ("#e6edfb", 1), "sky-dark": ("#19203a", 1),
        "candy": ("#fdeef5", 1), "candy-dark": ("#2a1a33", 1),
        "longhand": ("#e3d3ac", 1), "longhand-dark": ("#322a22", 1),
        "arcane-terminal": ("#1a1a2e", 1), "arcane-terminal-light": ("#e2e7ec", 1),
        "retro-16bit": ("#14110d", 1), "retro-16bit-light": ("#e0e0db", 1),
        "mario-8bit": ("#000000", 0.08), "mario-8bit-dark": ("#ffffff", 0.08),
        "risograph": ("#1a1612", 0.06), "risograph-dark": ("#f4efe1", 0.06),
        "mixtape": ("#2a1a0e", 0.06), "mixtape-dark": ("#e8c66a", 0.08),
        "mono": ("#000000", 0.05), "mono-dark": ("#ffffff", 0.04),
        "cartoon": ("#0a0a08", 0.06), "cartoon-dark": ("#fff5e1", 0.06),
        "jarvis": ("#0f1a2e", 1), "jarvis-light": ("#dceaf5", 1)
    ]

    /// The Web `--text-on-accent` hex for a variant, `nil` for an unknown id.
    static func accentForegroundHex(for theme: String) -> String? {
        themeAccentForegrounds[theme]
    }

    /// The Web `--bg-soft` token for a variant, `nil` for an unknown id.
    static func softBackgroundToken(for theme: String) -> (hex: String, opacity: Double)? {
        themeSoftBackgrounds[theme]
    }

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

    /// The pastel families lead the picker; Sorbet is the default.
    let availableThemeFamilies: [ThemeFamily] = [
        ThemeFamily(name: "Sorbet", lightTheme: "sorbet", darkTheme: "sorbet-dark"),
        ThemeFamily(name: "Sky", lightTheme: "sky", darkTheme: "sky-dark"),
        ThemeFamily(name: "Candy", lightTheme: "candy", darkTheme: "candy-dark"),
        ThemeFamily(name: "Longhand", lightTheme: "longhand", darkTheme: "longhand-dark"),
        ThemeFamily(name: "Arcane Terminal", lightTheme: "arcane-terminal-light", darkTheme: "arcane-terminal"),
        ThemeFamily(name: "Retro 16-bit", lightTheme: "retro-16bit-light", darkTheme: "retro-16bit"),
        ThemeFamily(name: "Mario 8-bit", lightTheme: "mario-8bit", darkTheme: "mario-8bit-dark"),
        ThemeFamily(name: "Risograph", lightTheme: "risograph", darkTheme: "risograph-dark"),
        ThemeFamily(name: "Mixtape", lightTheme: "mixtape", darkTheme: "mixtape-dark"),
        ThemeFamily(name: "Mono", lightTheme: "mono", darkTheme: "mono-dark"),
        ThemeFamily(name: "2D Cartoon", lightTheme: "cartoon", darkTheme: "cartoon-dark"),
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
    
    /// A saved theme (Longhand included) is kept; only a reader with no saved
    /// choice gets `defaultThemeID`. A saved retired id migrates to Sorbet
    /// (see `migratedThemeID`) and the migrated id is persisted.
    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        if let raw = defaults.string(forKey: "magican-appearance-mode"),
           let mode = AppearanceMode(rawValue: raw) {
            appearanceMode = mode
        }
        applyTheme(defaults.string(forKey: "magican-theme") ?? Self.defaultThemeID)
    }
    
    /// Apply a theme variant. `persistToAccount` distinguishes an explicit
    /// choice (family picked, Day/Night forced) — which pushes to the account
    /// through `themeSyncSink` — from the implicit `.system` re-resolution that
    /// follows the device's appearance and must not rewrite the account's
    /// record on every sunset.
    func applyTheme(_ requested: String, persistToAccount: Bool = true) {
        let name = Self.migratedThemeID(requested)
        applyPalette(name)
        fontNames = Self.fonts(for: name)
        isDark = Self.luminance(of: backgroundColor) < 0.5
        currentThemeName = name
        defaults.set(name, forKey: "magican-theme")
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

        if persistToAccount {
            themeSyncSink?(name, appearanceMode)
        }
    }

    /// Adopt the account's stored theme (the same `/ui/preferences` record the
    /// web follows). An unknown id leaves the local choice alone. The stored
    /// variant names the family; the stored mode (when a mobile surface wrote
    /// one) says how that family follows the device — a web-only record has no
    /// mode, so the variant's own day/night side stands in.
    ///
    /// The account is the truth once a device has met it (the web's rule). The
    /// FIRST meeting makes one exception so an established local pick is not
    /// silently replaced by a web-seeded default: unless the account holds a
    /// deliberate non-default record, this device declares its own choice once.
    func adoptAccountTheme(_ raw: String, appearance: String?, systemDark: Bool) {
        let seededKey = "magican-theme-account-seeded"
        if !defaults.bool(forKey: seededKey) {
            defaults.set(true, forKey: seededKey)
            let deliberate = raw != Self.defaultThemeID || appearance != nil
            if !deliberate, currentThemeName != Self.defaultThemeID {
                themeSyncSink?(currentThemeName, appearanceMode)
                return
            }
        }
        let name = Self.migratedThemeID(raw)
        guard let family = availableThemeFamilies.first(where: { $0.lightTheme == name || $0.darkTheme == name })
        else { return }
        let mode: AppearanceMode
        switch appearance {
        case "system": mode = .system
        case "day": mode = .day
        case "night": mode = .night
        default: mode = (family.darkTheme == name) ? .night : .day
        }
        guard family.id != currentThemeFamilyID || mode != appearanceMode else { return }
        appearanceMode = mode
        defaults.set(mode.rawValue, forKey: "magican-appearance-mode")
        // Converging on the account's record must not echo it back.
        applyTheme(
            resolvedDark(systemDark: systemDark) ? family.darkTheme : family.lightTheme,
            persistToAccount: false
        )
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
        defaults.set(mode.rawValue, forKey: "magican-appearance-mode")
        guard let family = themeFamily(containing: currentThemeName) else { return }
        applyTheme(resolvedDark(systemDark: systemDark) ? family.darkTheme : family.lightTheme)
    }

    /// Re-apply UIKit chrome (nav/tab bars, window traits) without changing the
    /// palette. The live-bar walk in `applySystemAppearance` only reaches bars
    /// attached to a window, so a bar that was detached when the theme flipped —
    /// a background tab's `UINavigationBar` — keeps the stale palette until it is
    /// re-styled. Re-running the walk when a tab becomes selected catches it.
    func refreshSystemAppearance() {
        applySystemAppearance()
        // Tab attachment completes on the next layout pass; re-apply after it so
        // a just-attached bar cannot be missed (same pattern as `applyTheme`).
        DispatchQueue.main.async { [weak self] in
            self?.applySystemAppearance()
        }
    }

    /// Called by the app root when the iOS appearance flips — re-applies the family's
    /// variant only when the app is following the system. Implicit by nature:
    /// the account's record keeps the family and mode, not this device's sunset.
    func systemAppearanceChanged(dark: Bool) {
        guard appearanceMode == .system, let family = themeFamily(containing: currentThemeName) else { return }
        let want = dark ? family.darkTheme : family.lightTheme
        if want != currentThemeName { applyTheme(want, persistToAccount: false) }
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

    /// Every variant's palette, keyed by theme id. Values mirror the Web
    /// `[data-theme]` blocks in `ui/unified-ui/src/app.css`.
    static let palettes: [String: PaletteTokens] = [
        "sorbet": .init(
            background: "#fff9f4", surface: "#fff4ec", elevated: "#ffffff",
            text: "#2b2230", secondaryText: "#5a4e60",
            accent: "#e8505f", accentHover: "#d63e4e",
            secondaryAccent: "#7a5cf0", wash: ["#ffe1e4", "#ece5ff", "#dcf5ea", "#fff9f4"]),
        "sorbet-dark": .init(
            background: "#1d1622", surface: "#251c2b", elevated: "#332839",
            text: "#f6ecf4", secondaryText: "#d6c6d6",
            accent: "#ff8a94", accentHover: "#ff9fa8",
            accentFill: "#ed0014", accentFillPressed: "#d40012",
            secondaryAccent: "#b9a3ff", wash: ["#4a2530", "#36295c", "#1d4235", "#1d1622"]),
        "sky": .init(
            background: "#f7f9ff", surface: "#eef3ff", elevated: "#ffffff",
            text: "#1e2440", secondaryText: "#454d6e",
            accent: "#5a68f0", accentHover: "#4a57e0",
            secondaryAccent: "#3d8fe0", wash: ["#e4e8ff", "#dcecff", "#ffe3ef", "#f7f9ff"]),
        "sky-dark": .init(
            background: "#0f1424", surface: "#161d33", elevated: "#222b4a",
            text: "#eef1ff", secondaryText: "#c3cae6",
            accent: "#8e9aff", accentHover: "#a3adff",
            accentFill: "#5163ff", accentFillPressed: "#3c51ff",
            secondaryAccent: "#8cc4ff", wash: ["#262f63", "#1c3558", "#4a2438", "#0f1424"]),
        "candy": .init(
            background: "#fffdf8", surface: "#fff6fb", elevated: "#ffffff",
            text: "#2a1a2e", secondaryText: "#5a4560",
            accent: "#e0418f", accentHover: "#cc307d",
            secondaryAccent: "#8a5cf0", wash: ["#ffd6ea", "#cdf5e4", "#fff3b8", "#fffdf8"]),
        "candy-dark": .init(
            background: "#1a1020", surface: "#23152b", elevated: "#331f3e",
            text: "#fbeef6", secondaryText: "#dcc6d8",
            accent: "#ff8fc7", accentHover: "#ffa6d3",
            accentFill: "#e60073", accentFillPressed: "#cd0067",
            secondaryAccent: "#c3a8ff", wash: ["#4d1f36", "#17443a", "#4a4214", "#1a1020"]),
        "arcane-terminal": .init(
            background: "#0a0a0f", surface: "#12121a", elevated: "#1a1a2e",
            text: "#e0e0e0", secondaryText: "#b0b0b0",
            accent: "#00d4aa", accentHover: "#00f5c4",
            accentFill: "#00856b", accentFillPressed: "#007760"),
        "arcane-terminal-light": .init(
            background: "#f6f8fa", surface: "#eef1f4", elevated: "#ffffff",
            text: "#0a0a0f", secondaryText: "#2a2a35",
            accent: "#007a66", accentHover: "#00604f"),
        "retro-16bit": .init(
            background: "#0c0a08", surface: "#14110d", elevated: "#1a1610",
            text: "#ffb000", secondaryText: "#ffcc00",
            accent: "#ffb000", accentHover: "#ffd000",
            accentFill: "#9e6d00", accentFillPressed: "#8c6000"),
        "retro-16bit-light": .init(
            background: "#f5f5f0", surface: "#ebebe6", elevated: "#ffffff",
            text: "#1a1a1a", secondaryText: "#333333",
            accent: "#1a1a1a", accentHover: "#000000"),
        "mario-8bit": .init(
            background: "#5c94fc", surface: "#88b8fc", elevated: "#ffffff",
            text: "#000000", secondaryText: "#1a1a1a",
            accent: "#e40000", accentHover: "#b80000"),
        "mario-8bit-dark": .init(
            background: "#000000", surface: "#181818", elevated: "#2a2a2a",
            text: "#ffffff", secondaryText: "#e0e0e0",
            accent: "#fbd000", accentHover: "#ffe040",
            accentFill: "#8b7300", accentFillPressed: "#7d6700"),
        "risograph": .init(
            background: "#f5efe1", surface: "#ede5d2", elevated: "#fbf6e8",
            text: "#1a1612", secondaryText: "#2d2820",
            accent: "#ff48b0", accentHover: "#e02e90"),
        "risograph-dark": .init(
            background: "#14110d", surface: "#1f1a14", elevated: "#2a2218",
            text: "#f4efe1", secondaryText: "#d8d2c1",
            accent: "#ff48b0", accentHover: "#ff70c4",
            accentFill: "#e40081", accentFillPressed: "#cb0074"),
        "mixtape": .init(
            background: "#e9d59f", surface: "#dcc692", elevated: "#f4e4b3",
            text: "#2a1a0e", secondaryText: "#3e2a18",
            accent: "#c8362a", accentHover: "#a82418"),
        "mixtape-dark": .init(
            background: "#0f0e0c", surface: "#1a1815", elevated: "#2a2520",
            text: "#e8c66a", secondaryText: "#d8b358",
            accent: "#ff5040", accentHover: "#ff6e60",
            accentFill: "#eb1400", accentFillPressed: "#d11200"),
        "mono": .init(
            background: "#f4f4f5", surface: "#e4e4e7", elevated: "#ffffff",
            text: "#000000", secondaryText: "#1a1a1a",
            accent: "#000000", accentHover: "#1a1a1a"),
        "mono-dark": .init(
            background: "#000000", surface: "#0a0a0a", elevated: "#141414",
            text: "#ffffff", secondaryText: "#e5e5e5",
            accent: "#ffffff", accentHover: "#e5e5e5",
            accentFill: "#757575", accentFillPressed: "#686868"),
        "cartoon": .init(
            background: "#9dd5ff", surface: "#84c8ff", elevated: "#ffffff",
            text: "#0a0a08", secondaryText: "#1a1a14",
            accent: "#ff5499", accentHover: "#ff3886"),
        "cartoon-dark": .init(
            background: "#1a1830", surface: "#25224a", elevated: "#2c2956",
            text: "#fff5e1", secondaryText: "#ede4ce",
            accent: "#ff7eb6", accentHover: "#ff9ec8",
            accentFill: "#e90065", accentFillPressed: "#cf005a"),
        "longhand-dark": .init(
            background: "#1a1612", surface: "#221d18", elevated: "#2a241e",
            text: "#f3ead6", secondaryText: "#d6c8a8",
            accent: "#d8602e", accentHover: "#e87a4a",
            accentFill: "#c35324", accentFillPressed: "#ae4a21"),
        "jarvis": .init(
            background: "#050a14", surface: "#0a1220", elevated: "#0e1828",
            text: "#d8ecff", secondaryText: "#98b6d6",
            accent: "#00d4ff", accentHover: "#33dfff",
            accentFill: "#00819c", accentFillPressed: "#00728a"),
        "jarvis-light": .init(
            background: "#f0f7ff", surface: "#e6f1fa", elevated: "#ffffff",
            text: "#062035", secondaryText: "#2d4a6b",
            accent: "#0099cc", accentHover: "#00b3e8"),
        "longhand": .init(
            background: "#f3ead6", surface: "#ede1c4", elevated: "#faf3e0",
            text: "#1a1612", secondaryText: "#3a2f24",
            accent: "#a04020", accentHover: "#732c14"),
    ]

    /// The palette for a theme id; unknown ids fall back to Longhand.
    static func palette(for name: String) -> PaletteTokens {
        palettes[name] ?? palettes["longhand"]!
    }

    /// True when `name` has its own palette entry (rather than the fallback).
    static func hasExplicitPalette(_ name: String) -> Bool {
        palettes[name] != nil
    }

    private func applyPalette(_ name: String) {
        let palette = Self.palette(for: name)
        backgroundColor = Color(hex: palette.background)
        surfaceColor = Color(hex: palette.surface)
        elevatedColor = Color(hex: palette.elevated)
        textColor = Color(hex: palette.text)
        secondaryTextColor = Color(hex: palette.secondaryText)
        accentColor = Color(hex: palette.accent)
        accentHoverColor = Color(hex: palette.accentHover)
        accentFillColor = Color(hex: palette.resolvedAccentFill)
        accentFillPressedColor = Color(hex: palette.resolvedAccentFillPressed)
        // `discoveryColor` reads `isDark`, which `applyTheme` sets after the
        // palette; derive the fallback from this palette's own background.
        if let secondary = palette.secondaryAccent {
            secondaryAccentColor = Color(hex: secondary)
        } else {
            secondaryAccentColor = Color(hex: Self.luminance(of: backgroundColor) < 0.5 ? "#d3a6ff" : "#7040a0")
        }
    }
}
