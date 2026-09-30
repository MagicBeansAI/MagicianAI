import SwiftUI
import UIKit

/// One resolved theme variant (light or dark) for the keyboard.
struct KeyboardPalette: Equatable {
    var background: Color
    var surface: Color
    var elevated: Color
    var text: Color
    var secondaryText: Color
    var accent: Color
    var isDark: Bool

    /// The app's default theme (Longhand) — used before the app has published a
    /// palette (e.g. keyboard used before the app was first opened).
    static let fallbackLight = KeyboardPalette(
        background: color("#f3ead6"), surface: color("#ede1c4"), elevated: color("#faf3e0"),
        text: color("#1a1612"), secondaryText: color("#3a2f24"), accent: color("#a04020"), isDark: false)
    static let fallbackDark = KeyboardPalette(
        background: color("#1a1612"), surface: color("#221d18"), elevated: color("#2a241e"),
        text: color("#f3ead6"), secondaryText: color("#d6c8a8"), accent: color("#d8602e"), isDark: true)

    fileprivate func encoded() -> [String: String] {
        [
            "background": KeyboardPalette.hex(background),
            "surface": KeyboardPalette.hex(surface),
            "elevated": KeyboardPalette.hex(elevated),
            "text": KeyboardPalette.hex(text),
            "secondaryText": KeyboardPalette.hex(secondaryText),
            "accent": KeyboardPalette.hex(accent),
            "isDark": isDark ? "1" : "0",
        ]
    }

    // MARK: hex <-> Color (local helpers; not a Color extension by design)

    static func color(_ raw: String) -> Color {
        let hex = raw.trimmingCharacters(in: CharacterSet.alphanumerics.inverted)
        var int: UInt64 = 0
        Scanner(string: hex).scanHexInt64(&int)
        let r, g, b: UInt64
        switch hex.count {
        case 3: (r, g, b) = ((int >> 8) * 17, (int >> 4 & 0xF) * 17, (int & 0xF) * 17)
        case 6: (r, g, b) = (int >> 16, int >> 8 & 0xFF, int & 0xFF)
        case 8: (r, g, b) = (int >> 16 & 0xFF, int >> 8 & 0xFF, int & 0xFF)
        default: (r, g, b) = (0, 0, 0)
        }
        return Color(.sRGB, red: Double(r) / 255, green: Double(g) / 255, blue: Double(b) / 255, opacity: 1)
    }

    private static func hex(_ c: Color) -> String {
        var r: CGFloat = 0, g: CGFloat = 0, b: CGFloat = 0, a: CGFloat = 0
        UIColor(c).getRed(&r, green: &g, blue: &b, alpha: &a)
        return String(format: "#%02x%02x%02x", Int(r * 255), Int(g * 255), Int(b * 255))
    }
}

// Defined in an extension so the struct keeps its synthesized memberwise init.
extension KeyboardPalette {
    fileprivate init?(decoded d: [String: String]) {
        guard let bg = d["background"], let sf = d["surface"], let el = d["elevated"],
              let tx = d["text"], let sec = d["secondaryText"], let ac = d["accent"] else { return nil }
        self.init(background: KeyboardPalette.color(bg), surface: KeyboardPalette.color(sf),
                  elevated: KeyboardPalette.color(el), text: KeyboardPalette.color(tx),
                  secondaryText: KeyboardPalette.color(sec), accent: KeyboardPalette.color(ac),
                  isDark: d["isDark"] == "1")
    }
}

/// The chosen theme family's *both* variants, shared with the keyboard through the
/// App Group. The keyboard picks light vs dark by the **system** appearance (iOS
/// day/night), independent of the app's own light/dark mode.
enum KeyboardThemeStore {
    private static var store: UserDefaults? { UserDefaults(suiteName: MagicianAccess.appGroup) }
    private static let key = "keyboard.theme.pair.v2"
    private static let appearanceKey = "keyboard.system.appearance.dark.v1"

    /// Called by the app whenever the active theme family changes.
    static func save(light: KeyboardPalette, dark: KeyboardPalette) {
        store?.set(["light": light.encoded(), "dark": dark.encoded()], forKey: key)
    }

    /// The last system appearance the keyboard actually observed while its view was
    /// in the window. Seeded into the FIRST paint on the next cold start so the
    /// keyboard opens in the right day/night variant instead of flashing light→dark:
    /// a keyboard extension's `traitCollection` doesn't reliably report dark mode in
    /// `viewDidLoad`, so we trust the last in-window observation over the early trait.
    /// `nil` until the keyboard has run once with a trustworthy trait.
    static var lastKnownDark: Bool? {
        get {
            guard let store, store.object(forKey: appearanceKey) != nil else { return nil }
            return store.bool(forKey: appearanceKey)
        }
        set {
            guard let newValue else { return }
            store?.set(newValue, forKey: appearanceKey)
        }
    }

    /// The variant matching the current system appearance (`dark`), with fallback.
    static func palette(dark: Bool) -> KeyboardPalette {
        guard let root = store?.dictionary(forKey: key) as? [String: [String: String]],
              let variant = root[dark ? "dark" : "light"],
              let palette = KeyboardPalette(decoded: variant) else {
            return dark ? .fallbackDark : .fallbackLight
        }
        return palette
    }
}
