import XCTest
@testable import Magician

/// Settings search (device rows + the embedded web Settings row) and where
/// the embedded web Settings loads from and what may load inside it.
final class SettingsSearchTests: XCTestCase {
    // MARK: Search

    func testEmptyQueryShowsEveryRow() {
        XCTAssertEqual(SettingsSearch.visibleRows(for: ""), SettingsSearchRow.allCases)
        XCTAssertEqual(SettingsSearch.visibleRows(for: "   "), SettingsSearchRow.allCases)
        XCTAssertFalse(SettingsSearch.isActive("  "))
    }

    func testEveryWordMustMatchTitleOrKeywordsIgnoringCaseAndAccents() {
        XCTAssertTrue(SettingsSearch.matches("LOCK screen", title: "Notifications", keywords: ["lock screen"]))
        XCTAssertTrue(SettingsSearch.matches("notif", title: "Notifications", keywords: []))
        XCTAssertTrue(SettingsSearch.matches("café", title: "Cafe", keywords: []))
        XCTAssertFalse(SettingsSearch.matches("lock widget", title: "Notifications", keywords: ["lock screen"]))
    }

    func testSettingsThatMovedToTheWebLeadToTheWebSettingsRow() {
        for query in ["budget", "Preferences", "voices", "workspace", "model", "harness credentials",
                      "storage", "trust", "rules", "engines", "notes", "devices", "spend"] {
            XCTAssertTrue(SettingsSearch.visibleRows(for: query).contains(.webSettings), query)
        }
        XCTAssertEqual(SettingsSearch.visibleRows(for: "budget"), [.webSettings])
    }

    func testDeviceRowsAreFoundByTheirOwnWords() {
        XCTAssertTrue(SettingsSearch.visibleRows(for: "siri").contains(.talkToProduct))
        XCTAssertTrue(SettingsSearch.visibleRows(for: "wake word").contains(.talkBehavior))
        XCTAssertTrue(SettingsSearch.visibleRows(for: "lock screen").contains(.notificationPermissions))
        XCTAssertTrue(SettingsSearch.visibleRows(for: "widget").contains(.atAGlance))
        XCTAssertTrue(SettingsSearch.visibleRows(for: "learned words").contains(.keyboardReset))
        XCTAssertTrue(SettingsSearch.visibleRows(for: "qr").contains(.selfHostedConnection))
        XCTAssertTrue(SettingsSearch.visibleRows(for: "version").contains(.about))
        XCTAssertTrue(SettingsSearch.visibleRows(for: "ios version").contains(.about))
        XCTAssertTrue(SettingsSearch.visibleRows(for: "magicutor version").contains(.about))
        XCTAssertTrue(SettingsSearch.visibleRows(for: "desktop version").contains(.about))
        XCTAssertTrue(SettingsSearch.visibleRows(for: "how to").contains(.howToUse))
    }

    func testNothingMatchesNonsense() {
        XCTAssertTrue(SettingsSearch.visibleRows(for: "zzqx").isEmpty)
    }

    func testEveryRowBelongsToADisplayedSection() {
        for row in SettingsSearchRow.allCases {
            XCTAssertFalse(row.title.isEmpty)
            XCTAssertFalse(row.keywords.isEmpty)
        }
        let sections = Set(SettingsSearchRow.allCases.map(\.section))
        XCTAssertEqual(sections.count, SettingsSearchSection.allCases.count)
    }

    // MARK: Section chip bar

    func testChipBarOffersEveryHeadedSectionWithoutAQuery() {
        XCTAssertEqual(
            SettingsSearch.visibleSections(for: ""),
            [.voice, .notifications, .keyboard, .connection, .help]
        )
        XCTAssertFalse(SettingsSearchSection.navigable.contains(.top))
    }

    func testChipBarShowsOnlySectionsWithMatches() {
        XCTAssertEqual(SettingsSearch.visibleSections(for: "siri"), [.voice])
        XCTAssertTrue(SettingsSearch.visibleSections(for: "version").contains(.help))
        XCTAssertFalse(SettingsSearch.visibleSections(for: "version").contains(.keyboard))
        // Only the top web Settings row matches: no chips, so the bar hides.
        XCTAssertEqual(SettingsSearch.visibleSections(for: "budget"), [])
        XCTAssertEqual(SettingsSearch.visibleSections(for: "zzqx"), [])
    }

    private let allSections: [SettingsSearchSection] = [.voice, .notifications, .keyboard, .connection, .help]

    private func active(
        _ tops: [SettingsSearchSection: CGFloat],
        order: [SettingsSearchSection]? = nil,
        end: Bool = false,
        current: SettingsSearchSection? = nil
    ) -> SettingsSearchSection? {
        SettingsSearch.activeSection(
            order: order ?? allSections,
            headerTops: tops,
            threshold: 200,
            reachedEnd: end,
            current: current
        )
    }

    func testActiveSectionIsTheLastHeadingScrolledToTheTop() {
        // At rest: the first heading sits below the bar, still the first section.
        XCTAssertEqual(active([.voice: 320, .notifications: 900]), .voice)
        XCTAssertEqual(active([.voice: -400, .notifications: 150, .keyboard: 600]), .notifications)
        XCTAssertEqual(active([.notifications: 40, .keyboard: 190, .connection: 700]), .keyboard)
    }

    func testContentAboveTheFirstVisibleHeadingBelongsToThePreviousSection() {
        XCTAssertEqual(active([.notifications: 700]), .voice)
        XCTAssertEqual(active([.connection: 500, .help: 900]), .keyboard)
    }

    func testInsideALongSectionKeepsTheCurrentChip() {
        XCTAssertEqual(active([:], current: .voice), .voice)
        XCTAssertEqual(active([:], current: nil), .voice)
        // A section hidden by search is never kept.
        XCTAssertEqual(active([:], order: [.keyboard, .help], current: .voice), .keyboard)
    }

    func testListEndSelectsTheLastSectionUnlessEverythingFits() {
        XCTAssertEqual(active([.connection: 120, .help: 500], end: true), .help)
        // All content fits: the first heading is still on screen.
        XCTAssertEqual(active([.voice: 300, .help: 500], order: [.voice, .help], end: true), .voice)
        XCTAssertNil(active([:], order: []))
    }

    // MARK: Embedded web Settings origin

    private func profile(_ origin: String) throws -> MobileConnectionProfile {
        try MobileConnectionProfile(
            publicOrigin: try XCTUnwrap(URL(string: origin)),
            principal: "owner",
            workspace: "default",
            deviceID: "device-1",
            deviceToken: "token-1"
        )
    }

    private func machine(api: String, ui: String, isSelf: Bool = false) -> RemoteMachine {
        RemoteMachine(name: "studio", apiOrigin: api, uiOrigin: ui, isSelf: isSelf)
    }

    func testUsesTheRegistryUIOriginForThePairedMachine() throws {
        let paired = try profile("https://api.example.com")
        let origin = EmbeddedWebSettings.webOrigin(
            profile: paired,
            registry: [machine(api: "https://api.example.com", ui: "https://app.example.com/")]
        )
        XCTAssertEqual(origin.absoluteString, "https://app.example.com")
        XCTAssertEqual(
            EmbeddedWebSettings.settingsURL(origin: origin)?.absoluteString,
            "https://app.example.com/settings?embed=1&host=ios"
        )
    }

    func testFallsBackToThePairedOriginWhenTheUIOriginIsUnusable() throws {
        let remote = try profile("https://api.example.com")
        XCTAssertEqual(EmbeddedWebSettings.webOrigin(profile: remote, registry: []), remote.publicOrigin)
        // Loopback means this iPhone, not the machine.
        XCTAssertEqual(
            EmbeddedWebSettings.webOrigin(
                profile: remote,
                registry: [machine(api: "https://api.example.com", ui: "https://localhost:5173")]
            ),
            remote.publicOrigin
        )
        // A private-network page is unreachable over the remote route.
        XCTAssertEqual(
            EmbeddedWebSettings.webOrigin(
                profile: remote,
                registry: [machine(api: "https://api.example.com", ui: "http://192.168.1.20:5173")]
            ),
            remote.publicOrigin
        )
        let lan = try profile("http://192.168.1.20:8080")
        XCTAssertEqual(
            EmbeddedWebSettings.webOrigin(
                profile: lan,
                registry: [machine(api: "http://192.168.1.99:8080", ui: "http://192.168.1.20:5173", isSelf: true)]
            ).absoluteString,
            "http://192.168.1.20:5173"
        )
    }

    // MARK: Embedded web Settings navigation

    private let app = URL(string: "https://app.example.com/settings?embed=1&host=ios")!

    private func decide(
        _ raw: String,
        mainFrame: Bool = true,
        link: Bool = true,
        current: String? = "https://app.example.com/settings?embed=1&host=ios"
    ) -> EmbeddedWebSettings.Decision {
        EmbeddedWebSettings.decide(
            url: URL(string: raw),
            appOrigin: app,
            isMainFrame: mainFrame,
            isLinkActivated: link,
            currentURL: current.flatMap { URL(string: $0) }
        )
    }

    func testSettingsAndLoginStayInsideTheEmbeddedView() {
        XCTAssertEqual(decide("https://app.example.com/settings?embed=1&host=ios"), .allow)
        XCTAssertEqual(decide("https://app.example.com/login?redirectTo=%2Fsettings"), .allow)
        XCTAssertEqual(decide("https://app.example.com/cdn-cgi/access/authorized?token=x"), .allow)
        XCTAssertEqual(decide("about:blank"), .allow)
        XCTAssertEqual(decide("https://cdn.example.net/font.woff", mainFrame: false), .allow)
    }

    func testOtherPagesAndExternalLinksOpenInSafari() {
        XCTAssertEqual(decide("https://app.example.com/chat"), .openExternally)
        XCTAssertEqual(decide("https://app.example.com/"), .openExternally)
        XCTAssertEqual(decide("https://docs.example.org/guide"), .openExternally)
        XCTAssertEqual(decide("mailto:owner@example.com"), .openExternally)
        // Same host, different port is a different origin.
        XCTAssertEqual(decide("https://app.example.com:8443/settings"), .openExternally)
    }

    func testCloudflareAccessSignInLoadsInside() {
        XCTAssertEqual(decide("https://team.cloudflareaccess.com/cdn-cgi/access/login/app", link: false), .allow)
        XCTAssertEqual(decide("https://team.cloudflareaccess.com/cdn-cgi/access/login/app"), .allow)
        // Identity-provider pages reached from the sign-in page.
        XCTAssertEqual(
            decide("https://accounts.example-idp.com/signin", current: "https://team.cloudflareaccess.com/login"),
            .allow
        )
        // A redirect (not a tapped link) off the origin is part of a sign-in.
        XCTAssertEqual(decide("https://accounts.example-idp.com/signin", link: false), .allow)
    }

    func testCancelledLoadsAreNotFailures() {
        XCTAssertTrue(EmbeddedWebSettings.isBenignLoadError(URLError(.cancelled)))
        XCTAssertTrue(EmbeddedWebSettings.isBenignLoadError(NSError(domain: "WebKitErrorDomain", code: 102)))
        XCTAssertFalse(EmbeddedWebSettings.isBenignLoadError(URLError(.notConnectedToInternet)))
    }
}
