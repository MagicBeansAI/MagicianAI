import SwiftUI
import UIKit
import XCTest
@testable import Magician

/// Sorbet, Sky and Candy: the web's pastel families, ported from
/// `ui/unified-ui/src/app.css`. Sorbet is the default for a reader with no
/// saved choice; a saved theme is kept.
final class LivelyThemeTests: XCTestCase {
    private var suiteName: String!
    private var defaults: UserDefaults!

    override func setUp() {
        super.setUp()
        suiteName = "LivelyThemeTests.\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suiteName)
    }

    override func tearDown() {
        defaults.removePersistentDomain(forName: suiteName)
        defaults = nil
        super.tearDown()
    }

    func testPastelFamiliesAreListedFirstWithSorbetDefault() {
        let theme = ThemeManager(defaults: defaults)
        let families = theme.availableThemeFamilies
        XCTAssertEqual(families.prefix(3).map(\.lightTheme), ["sorbet", "sky", "candy"])
        XCTAssertEqual(families.prefix(3).map(\.darkTheme), ["sorbet-dark", "sky-dark", "candy-dark"])
        XCTAssertEqual(families.prefix(3).map(\.name), ["Sorbet", "Sky", "Candy"])
        XCTAssertEqual(ThemeManager.defaultThemeID, "sorbet")
        XCTAssertEqual(families.count, 12)
        XCTAssertEqual(theme.availableThemes.count, 24)
    }

    func testEveryVariantHasExplicitPaletteFontsAccentForegroundAndSoftBackground() {
        let theme = ThemeManager(defaults: defaults)
        for id in theme.availableThemes {
            XCTAssertNotNil(ThemeManager.themeFonts[id], id)
            XCTAssertTrue(ThemeManager.hasExplicitPalette(id), id)
            XCTAssertNotNil(ThemeManager.accentForegroundHex(for: id), id)
            XCTAssertNotNil(ThemeManager.softBackgroundToken(for: id), id)
        }
        XCTAssertFalse(ThemeManager.hasExplicitPalette("no-such-theme"))
    }

    func testPastelPalettesMatchWebTokens() {
        let sorbet = ThemeManager.palette(for: "sorbet")
        XCTAssertEqual(sorbet.background, "#fff9f4")
        XCTAssertEqual(sorbet.surface, "#fff4ec")
        XCTAssertEqual(sorbet.elevated, "#ffffff")
        XCTAssertEqual(sorbet.text, "#2b2230")
        XCTAssertEqual(sorbet.secondaryText, "#5a4e60")
        XCTAssertEqual(sorbet.accent, "#e8505f")
        XCTAssertEqual(sorbet.accentHover, "#d63e4e")
        XCTAssertEqual(sorbet.secondaryAccent, "#7a5cf0")
        XCTAssertEqual(sorbet.wash, ["#ffe1e4", "#ece5ff", "#dcf5ea", "#fff9f4"])

        let sorbetDark = ThemeManager.palette(for: "sorbet-dark")
        XCTAssertEqual(sorbetDark.background, "#1d1622")
        XCTAssertEqual(sorbetDark.accent, "#ff8a94")
        XCTAssertEqual(sorbetDark.secondaryAccent, "#b9a3ff")
        XCTAssertEqual(sorbetDark.wash, ["#4a2530", "#36295c", "#1d4235", "#1d1622"])

        let sky = ThemeManager.palette(for: "sky")
        XCTAssertEqual(sky.background, "#f7f9ff")
        XCTAssertEqual(sky.accent, "#5a68f0")
        XCTAssertEqual(sky.secondaryAccent, "#3d8fe0")
        XCTAssertEqual(ThemeManager.palette(for: "sky-dark").elevated, "#222b4a")

        let candy = ThemeManager.palette(for: "candy")
        XCTAssertEqual(candy.background, "#fffdf8")
        XCTAssertEqual(candy.accent, "#e0418f")
        XCTAssertEqual(candy.wash, ["#ffd6ea", "#cdf5e4", "#fff3b8", "#fffdf8"])
        XCTAssertEqual(ThemeManager.palette(for: "candy-dark").text, "#fbeef6")

        XCTAssertEqual(ThemeManager.accentForegroundHex(for: "sorbet"), "#ffffff")
        XCTAssertEqual(ThemeManager.accentForegroundHex(for: "sorbet-dark"), "#ffffff")
        XCTAssertEqual(ThemeManager.accentForegroundHex(for: "sky-dark"), "#ffffff")
        XCTAssertEqual(ThemeManager.accentForegroundHex(for: "candy-dark"), "#ffffff")
        XCTAssertEqual(ThemeManager.softBackgroundToken(for: "candy")?.hex, "#fdeef5")
    }

    /// Dark variants fill on-accent surfaces with a deeper shade of their
    /// accent (same hue/saturation) that white text reads on at WCAG AA;
    /// light variants fill with the accent itself, so they look unchanged.
    func testAccentFillCarriesWhiteInDarkThemesAndEqualsAccentInLightThemes() throws {
        let theme = ThemeManager(defaults: defaults)
        for family in theme.availableThemeFamilies {
            let light = ThemeManager.palette(for: family.lightTheme)
            XCTAssertNil(light.accentFill, family.lightTheme)
            XCTAssertEqual(light.resolvedAccentFill, light.accent, family.lightTheme)
            XCTAssertEqual(light.resolvedAccentFillPressed, light.accentHover, family.lightTheme)

            let darkID = family.darkTheme
            let dark = ThemeManager.palette(for: darkID)
            let fill = try XCTUnwrap(dark.accentFill, "\(darkID) needs an accentFill")
            let pressed = try XCTUnwrap(dark.accentFillPressed, "\(darkID) needs an accentFillPressed")
            XCTAssertGreaterThanOrEqual(Self.whiteContrast(fill), 4.5, "\(darkID) fill \(fill)")
            XCTAssertGreaterThanOrEqual(Self.whiteContrast(pressed), 5.4, "\(darkID) pressed \(pressed)")
            XCTAssertLessThan(Self.whiteContrast(pressed), 5.8, "\(darkID) pressed \(pressed)")
            XCTAssertGreaterThan(Self.whiteContrast(pressed), Self.whiteContrast(fill), darkID)
            XCTAssertEqual(ThemeManager.accentForegroundHex(for: darkID), "#ffffff", darkID)
            // Same hue as the bright accent (grays have none to keep).
            let accentHSL = Self.hsl(dark.accent), fillHSL = Self.hsl(fill)
            if accentHSL.s > 0.05 {
                let delta = abs(accentHSL.h - fillHSL.h)
                XCTAssertLessThan(min(delta, 360 - delta), 3, "\(darkID) hue drifted")
            }

            // The live theme publishes the fill; the bright accent is kept.
            theme.applyTheme(darkID)
            XCTAssertTrue(theme.isDark, darkID)
            XCTAssertEqual(UIColor(theme.accentFillColor), UIColor(Color(hex: fill)), darkID)
            XCTAssertEqual(UIColor(theme.accentFillPressedColor), UIColor(Color(hex: pressed)), darkID)
            XCTAssertEqual(UIColor(theme.accentColor), UIColor(Color(hex: dark.accent)), darkID)
            XCTAssertEqual(UIColor(theme.onAccentColor), UIColor(Color.white), darkID)
            XCTAssertEqual(UIColor(theme.solidFill(for: theme.accentColor)), UIColor(theme.accentFillColor), darkID)

            theme.applyTheme(family.lightTheme)
            XCTAssertFalse(theme.isDark, family.lightTheme)
            XCTAssertEqual(UIColor(theme.accentFillColor), UIColor(theme.accentColor), family.lightTheme)
            XCTAssertEqual(UIColor(theme.accentFillPressedColor), UIColor(theme.accentHoverColor), family.lightTheme)
        }
    }

    private static func channels(_ hex: String) -> (Double, Double, Double) {
        let value = Int(hex.dropFirst(), radix: 16) ?? 0
        return (Double((value >> 16) & 0xff) / 255, Double((value >> 8) & 0xff) / 255, Double(value & 0xff) / 255)
    }

    private static func whiteContrast(_ hex: String) -> Double {
        func linear(_ c: Double) -> Double { c <= 0.04045 ? c / 12.92 : pow((c + 0.055) / 1.055, 2.4) }
        let (r, g, b) = channels(hex)
        let luminance = 0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
        return 1.05 / (luminance + 0.05)
    }

    private static func hsl(_ hex: String) -> (h: Double, s: Double) {
        let (r, g, b) = channels(hex)
        let maxC = max(r, g, b), minC = min(r, g, b), delta = maxC - minC
        let l = (maxC + minC) / 2
        guard delta > 0 else { return (0, 0) }
        let s = delta / (1 - abs(2 * l - 1))
        var h: Double
        if maxC == r { h = ((g - b) / delta).truncatingRemainder(dividingBy: 6) }
        else if maxC == g { h = (b - r) / delta + 2 }
        else { h = (r - g) / delta + 4 }
        h *= 60
        if h < 0 { h += 360 }
        return (h, s)
    }

    func testExistingPalettesAreUnchanged() {
        let longhand = ThemeManager.palette(for: "longhand")
        XCTAssertEqual(longhand.background, "#f3ead6")
        XCTAssertEqual(longhand.accentHover, "#732c14")
        XCTAssertNil(longhand.secondaryAccent)
        XCTAssertNil(longhand.wash)
        XCTAssertEqual(ThemeManager.palette(for: "jarvis").accent, "#00d4ff")
        XCTAssertEqual(ThemeManager.palette(for: "mario-8bit").surface, "#88b8fc")
        // Unknown ids keep the historical Longhand fallback.
        XCTAssertEqual(ThemeManager.palette(for: "no-such-theme"), longhand)
    }

    func testPastelFontsFollowTheWeb() {
        XCTAssertEqual(
            ThemeManager.fonts(for: "sorbet"),
            ThemeFontNames(brand: "Outfit", display: "Fredoka", body: "Nunito", mono: "Geist Mono"))
        XCTAssertEqual(ThemeManager.fonts(for: "sorbet-dark"), ThemeManager.fonts(for: "sorbet"))
        XCTAssertEqual(ThemeManager.fonts(for: "sky").display, "Outfit")
        XCTAssertEqual(ThemeManager.fonts(for: "sky-dark").body, "Nunito")
        XCTAssertEqual(ThemeManager.fonts(for: "candy").display, "Baloo 2")
        XCTAssertEqual(ThemeManager.fonts(for: "candy-dark").display, "Baloo 2")
    }

    /// Every family a theme names must resolve to a registered face, or the
    /// theme silently renders in the system font.
    func testEveryThemeFontFamilyIsRegistered() {
        let families = Set(ThemeManager.themeFonts.values.flatMap {
            [$0.brand, $0.display, $0.body, $0.mono]
        })
        for family in families.sorted() {
            XCTAssertFalse(UIFont.fontNames(forFamilyName: family).isEmpty, "\(family) is not registered (UIAppFonts)")
        }
    }

    func testUnsetPreferenceGetsSorbetAndFollowsSystemDark() {
        let theme = ThemeManager(defaults: defaults)
        XCTAssertEqual(theme.currentThemeName, "sorbet")
        XCTAssertEqual(theme.appearanceMode, .system)
        XCTAssertEqual(theme.currentThemeFamilyID, "sorbet")

        theme.systemAppearanceChanged(dark: true)
        XCTAssertEqual(theme.currentThemeName, "sorbet-dark")
        XCTAssertTrue(theme.isDark)
    }

    /// Bubbly and Soft Machine are retired on iOS: nothing lists, paints or
    /// fonts them any more.
    func testRetiredFamiliesAreGone() {
        let theme = ThemeManager(defaults: defaults)
        let retired = ["bubbly", "bubbly-dark", "soft-machine", "soft-machine-dark"]
        XCTAssertFalse(theme.availableThemeFamilies.contains { ["Bubbly", "Soft Machine"].contains($0.name) })
        for id in retired {
            XCTAssertFalse(theme.availableThemes.contains(id), id)
            XCTAssertFalse(ThemeManager.hasExplicitPalette(id), id)
            XCTAssertNil(ThemeManager.themeFonts[id], id)
            XCTAssertNil(ThemeManager.accentForegroundHex(for: id), id)
            XCTAssertNil(ThemeManager.softBackgroundToken(for: id), id)
        }
    }

    /// A persisted retired id moves to Sorbet, keeping its day/night side.
    func testPersistedRetiredThemeMigratesToSorbetKeepingItsSide() {
        let expected = [
            "bubbly": "sorbet", "bubbly-dark": "sorbet-dark",
            "soft-machine": "sorbet", "soft-machine-dark": "sorbet-dark"
        ]
        for (retired, sorbet) in expected {
            defaults.set(retired, forKey: "magican-theme")
            let theme = ThemeManager(defaults: defaults)
            XCTAssertEqual(theme.currentThemeName, sorbet, retired)
            XCTAssertEqual(theme.currentThemeFamilyID, "sorbet", retired)
            XCTAssertEqual(defaults.string(forKey: "magican-theme"), sorbet, retired)
            XCTAssertEqual(ThemeManager.migratedThemeID(retired), sorbet)

            // A late caller handing over an old id lands on the same variant.
            theme.applyTheme("longhand")
            theme.applyTheme(retired)
            XCTAssertEqual(theme.currentThemeName, sorbet, retired)
        }
        XCTAssertEqual(ThemeManager.migratedThemeID("longhand-dark"), "longhand-dark")
    }

    func testSavedThemeIsKept() {
        defaults.set("longhand", forKey: "magican-theme")
        XCTAssertEqual(ThemeManager(defaults: defaults).currentThemeName, "longhand")

        defaults.set("jarvis", forKey: "magican-theme")
        defaults.set("night", forKey: "magican-appearance-mode")
        let theme = ThemeManager(defaults: defaults)
        XCTAssertEqual(theme.currentThemeName, "jarvis")
        XCTAssertEqual(theme.appearanceMode, .night)
    }

    func testThemeChoiceIsWrittenToTheInjectedDefaults() {
        let theme = ThemeManager(defaults: defaults)
        theme.applyTheme("candy-dark")
        XCTAssertEqual(defaults.string(forKey: "magican-theme"), "candy-dark")
        theme.setAppearanceMode(.day, systemDark: true)
        XCTAssertEqual(defaults.string(forKey: "magican-appearance-mode"), "day")
        XCTAssertEqual(theme.currentThemeName, "candy")
    }

    func testAppliedPastelThemeDrivesLiveColors() {
        let theme = ThemeManager(defaults: defaults)
        theme.applyTheme("sky")
        XCTAssertEqual(UIColor(theme.backgroundColor), UIColor(Color(hex: "#f7f9ff")))
        XCTAssertEqual(UIColor(theme.secondaryAccentColor), UIColor(Color(hex: "#3d8fe0")))
        XCTAssertEqual(UIColor(theme.onAccentColor), UIColor(Color(hex: "#ffffff")))
        XCTAssertFalse(theme.isDark)
        XCTAssertEqual(theme.fontNames.body, "Nunito")

        // Themes without a web secondary accent keep a distinct, themed hue.
        theme.applyTheme("longhand")
        XCTAssertEqual(UIColor(theme.secondaryAccentColor), UIColor(theme.discoveryColor))
    }

    /// The keyboard extension reads both variants through `paletteColors(for:)`.
    func testKeyboardPaletteProbeResolvesPastelIds() {
        XCTAssertEqual(
            UIColor(ThemeManager.paletteColors(for: "sorbet-dark").background),
            UIColor(Color(hex: "#1d1622")))
        XCTAssertEqual(
            UIColor(ThemeManager.paletteColors(for: "candy").accent),
            UIColor(Color(hex: "#e0418f")))
    }
}
