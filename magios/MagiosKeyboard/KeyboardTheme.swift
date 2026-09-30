import SwiftUI
import UIKit

/// Visual tokens for the keyboard, sourced from the Magican app's active theme
/// (shared via the App Group as `KeyboardPalette`) so the keys, pills, and panels
/// match whatever theme the user picked in the app. Refreshed by the controller
/// each time the keyboard appears.
enum KeyboardTheme {
    /// The live palette. Cached (a stored static) so color reads are cheap; the
    /// controller calls `refreshPalette(dark:)` with the **system** appearance so the
    /// keyboard shows the day/night variant of the chosen theme family regardless of
    /// the app's own light/dark mode.
    static var palette: KeyboardPalette = .fallbackLight
    static func refreshPalette(dark: Bool) { palette = KeyboardThemeStore.palette(dark: dark) }

    static var accent: Color { palette.accent }
    static var accentSoft: Color { palette.accent.opacity(palette.isDark ? 0.24 : 0.16) }

    /// The keyboard base — the theme background.
    static var backdrop: Color { palette.background }
    /// A normal character key — the theme's elevated surface.
    static var keyFill: Color { palette.elevated }
    /// Shift / delete / layer / return — the theme's mid surface.
    static var functionFill: Color { palette.surface }
    /// Pressed feedback — a clear wash of the theme accent (stronger + it lingers a
    /// fraction after release; see KeyView).
    static var keyPressed: Color { palette.accent.opacity(palette.isDark ? 0.55 : 0.45) }
    static var keyText: Color { palette.text }
    static var keyShadow: Color { Color.black.opacity(palette.isDark ? 0.45 : 0.18) }

    static let cornerRadius: CGFloat = 6
    static let rowHeight: CGFloat = 52
    static let rowSpacing: CGFloat = 9
    static let keySpacing: CGFloat = 6
}
