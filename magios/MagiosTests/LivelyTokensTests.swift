import SwiftUI
import UIKit
import XCTest
@testable import Magician

/// Shared style tokens the lively chat cards read: radii, status-dot tones,
/// chip style and the background wash.
final class LivelyTokensTests: XCTestCase {
    private var suiteName: String!
    private var theme: ThemeManager!

    override func setUp() {
        super.setUp()
        suiteName = "LivelyTokensTests.\(UUID().uuidString)"
        theme = ThemeManager(defaults: UserDefaults(suiteName: suiteName)!)
    }

    override func tearDown() {
        UserDefaults(suiteName: suiteName)?.removePersistentDomain(forName: suiteName)
        theme = nil
        super.tearDown()
    }

    private func assertSame(_ lhs: Color, _ rhs: Color, _ message: String, line: UInt = #line) {
        XCTAssertEqual(UIColor(lhs), UIColor(rhs), message, line: line)
    }

    func testRadiiAndTapTarget() {
        XCTAssertEqual(Lively.chipRadius, 6)
        XCTAssertEqual(Lively.cardRadius, 14)
        XCTAssertEqual(Lively.composerRadius, 10)
        XCTAssertEqual(Lively.bubbleRadius, 16)
        XCTAssertEqual(Lively.tapMin, 44)
        XCTAssertGreaterThanOrEqual(ChipStyle.minHitSize, Lively.tapMin)
        XCTAssertEqual(ChipStyle.cornerRadius, Lively.chipRadius)
        XCTAssertEqual(ChipStyle.fontSize, 12)
    }

    /// Mirrors the web card: completed green, failed red, partial and waiting
    /// amber, not-started the secondary accent, cancelled muted.
    func testStatusDotToneMapsToThemeColors() {
        theme.applyTheme("sorbet")
        let expected: [(StatusDot.Tone, Color)] = [
            (.running, theme.statusRunningColor),
            (.waiting, theme.warningColor),
            (.done, theme.successColor),
            (.failed, theme.dangerColor),
            (.partial, theme.warningColor),
            (.notStarted, theme.secondaryAccentColor),
            (.cancelled, theme.secondaryTextColor)
        ]
        XCTAssertEqual(expected.count, StatusDot.Tone.allCases.count)
        for (tone, color) in expected {
            assertSame(StatusDot.color(for: tone, in: theme), color, "\(tone)")
        }
        assertSame(StatusDot.color(for: .running, in: theme), Color(hex: "#4a78e0"), "sorbet status-running blue, as the web card")
        assertSame(StatusDot.color(for: .notStarted, in: theme), Color(hex: "#7a5cf0"), "sorbet secondary")
    }

    func testOnlyLiveTonesAnimateAndReduceMotionStopsThem() {
        for tone in StatusDot.Tone.allCases {
            let dot = StatusDot(tone: tone)
            let live = tone == .running || tone == .waiting
            XCTAssertEqual(dot.animates(reduceMotion: false), live, "\(tone)")
            XCTAssertFalse(dot.animates(reduceMotion: true), "\(tone)")
        }
    }

    func testWashUsesThePaletteStopsOrFallsBackToBackgroundAndSurface() {
        XCTAssertEqual(
            ThemeManager.washHexStops(for: "sorbet"),
            ["#ffe1e4", "#ece5ff", "#dcf5ea", "#fff9f4"])
        XCTAssertEqual(
            ThemeManager.washHexStops(for: "candy-dark"),
            ["#4d1f36", "#17443a", "#4a4214", "#1a1020"])
        // Themes without a web wash still get a gradient, never one flat colour.
        XCTAssertEqual(ThemeManager.washHexStops(for: "longhand"), ["#f3ead6", "#ede1c4"])
        for id in theme.availableThemes {
            XCTAssertGreaterThanOrEqual(ThemeManager.washHexStops(for: id).count, 2, id)
        }
    }

    /// The web card's running dot is `--status-running` (`--color-info`) in
    /// every theme; a theme missing here would fall back to a generic blue.
    func testEveryThemeHasItsStatusRunningColour() {
        for id in ThemeManager.shared.availableThemes {
            XCTAssertNotNil(ThemeManager.themeStatusRunning[id], id)
        }
    }
}
