import Foundation
import XCTest
@testable import Magician

/// The Observe "Command Deck": header status line, KPI counts, pane deep links,
/// Sources counting + decoding, Recent parsing, audio preference encode/decode,
/// and the broadcast-arm / reattach rules behind the iOS bug fixes.
@MainActor
final class ObserveDeckTests: XCTestCase {
    private var sessions: [URLSession] = []

    override func setUp() {
        super.setUp()
        MockURLProtocol.handler = nil
    }

    override func tearDown() {
        sessions.forEach { $0.invalidateAndCancel() }
        sessions.removeAll()
        MockURLProtocol.handler = nil
        ObservationArm.clear()
        super.tearDown()
    }

    private func mockSession() -> URLSession {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [MockURLProtocol.self]
        let s = URLSession(configuration: config)
        sessions.append(s)
        return s
    }

    nonisolated private static func json(_ request: URLRequest, _ status: Int = 200, _ body: Any) throws -> (HTTPURLResponse, Data) {
        (
            HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil, headerFields: nil)!,
            try JSONSerialization.data(withJSONObject: body)
        )
    }

    private func metrics(
        active: [String] = [],
        inApp: String? = nil,
        inAppActive: Bool = false,
        liveMeetings: Int = 0,
        recent: Int? = nil,
        notes: Int? = nil
    ) -> ObserveDeckMetrics {
        ObserveDeckMetrics(
            activeSessionIds: active,
            inAppSessionId: inApp,
            inAppActive: inAppActive,
            liveMeetingCount: liveMeetings,
            sourcesOn: nil,
            audioSurfaces: nil,
            recentCount: recent,
            publishedNotesTotal: notes
        )
    }

    // MARK: status line + KPIs

    func testStatusLineCoversLiveMeetingOnlyAndQuiet() {
        XCTAssertEqual(metrics(active: ["a"]).statusLine, "1 capture live")
        XCTAssertEqual(metrics(active: ["a", "b"]).statusLine, "2 captures live")
        XCTAssertEqual(metrics(liveMeetings: 1).statusLine, "1 meeting live — nothing capturing")
        XCTAssertEqual(metrics(liveMeetings: 3).statusLine, "3 meetings live — nothing capturing")
        XCTAssertEqual(metrics().statusLine, "Quiet — nothing capturing")
        XCTAssertEqual(metrics(liveMeetings: 2).nowSub, "2 live meetings")
        XCTAssertEqual(metrics().nowSub, "Live captures & meetings")
    }

    func testActiveCaptureCountDedupesTheInAppSession() {
        // Listed by the server → counted once.
        XCTAssertEqual(metrics(active: ["s1", "s2"], inApp: "s1", inAppActive: true).activeCaptureCount, 2)
        // Starting (no session id yet) → counted on top of the server list.
        XCTAssertEqual(metrics(active: ["s2"], inApp: nil, inAppActive: true).activeCaptureCount, 2)
        // Not yet listed by the server → counted.
        XCTAssertEqual(metrics(active: [], inApp: "s1", inAppActive: true).activeCaptureCount, 1)
        XCTAssertEqual(metrics(active: [], inApp: nil, inAppActive: false).activeCaptureCount, 0)
    }

    func testKPIMetricTextAndAccessibility() {
        let m = ObserveDeckMetrics(
            activeSessionIds: ["x"], inAppSessionId: nil, inAppActive: false, liveMeetingCount: 0,
            sourcesOn: 4, audioSurfaces: nil, recentCount: nil, publishedNotesTotal: 12
        )
        XCTAssertEqual(m.metricText(.now), "1")
        XCTAssertEqual(m.metricText(.sources), "4")
        XCTAssertEqual(m.metricText(.audio), "—")
        // Notes falls back to the published-notes total when Recent is unavailable.
        XCTAssertEqual(m.metricText(.notes), "12")
        XCTAssertEqual(metrics(recent: 7, notes: 12).metricText(.notes), "7")
        XCTAssertEqual(m.accessibilityLabel(.now, selected: true), "Now and live, 1 live capture, selected")
        XCTAssertEqual(m.accessibilityLabel(.sources, selected: false), "Sources on, 4 sources on")
    }

    // MARK: panes + deep links

    func testPaneParsingFromDeepLinks() {
        XCTAssertEqual(ObservePane.deepLink(URL(string: "magican://observe?pane=sources")!), .some(.sources))
        XCTAssertEqual(ObservePane.deepLink(URL(string: "magican://observe?pane=AUDIO")!), .some(.audio))
        XCTAssertEqual(ObservePane.deepLink(URL(string: "magican://observe/notes")!), .some(.notes))
        // An Observe link with no / an unknown pane keeps the remembered view.
        XCTAssertEqual(ObservePane.deepLink(URL(string: "magican://observe")!), .some(nil))
        XCTAssertEqual(ObservePane.deepLink(URL(string: "magican://observe?pane=bogus")!), .some(nil))
        // Not an Observe link.
        XCTAssertTrue(ObservePane.deepLink(URL(string: "magican://thread/abc")!) == nil)
        XCTAssertTrue(ObservePane.deepLink(URL(string: "https://observe/?pane=now")!) == nil)
    }

    func testObserveViewRequestStoresThePaneWithoutStartingACapture() {
        let defaults = UserDefaults(suiteName: "ObserveDeckTests")!
        defaults.removePersistentDomain(forName: "ObserveDeckTests")
        let actions = AppActions.shared
        let before = actions.observeRequestID
        let viewBefore = actions.observeViewRequestID
        actions.requestObserveView(pane: .audio, defaults: defaults)
        XCTAssertEqual(defaults.string(forKey: ObservePane.storageKey), "audio")
        XCTAssertEqual(actions.observeViewRequestID, viewBefore + 1)
        XCTAssertEqual(actions.observeRequestID, before, "opening Observe must not start listening")
        actions.requestObserveView(pane: nil, defaults: defaults)
        XCTAssertEqual(defaults.string(forKey: ObservePane.storageKey), "audio")
    }

    // MARK: sources

    func testSourcesCountingAndDecoding() {
        let channels = ObserveChannel.list(from: ["channels": [
            ["provider": "gmail", "provider_display": "Gmail", "account_alias": "me", "display": "me@x.com",
             "enabled": true, "connected": true, "message_count": 42, "thread_count": 9, "lane": "personal",
             "purposes": ["verification_codes"]],
            ["provider": "whatsapp_kapso", "account_alias": "self", "enabled": false, "connected": false],
            ["account_alias": "no-provider"],
        ]])
        XCTAssertEqual(channels.count, 2)
        XCTAssertEqual(channels[0].providerLabel, "Gmail")
        XCTAssertEqual(channels[0].account, "me@x.com")
        XCTAssertTrue(channels[0].verificationCodes)
        XCTAssertEqual(channels[1].providerLabel, "Whatsapp Kapso")

        let calendar = ObserveCalendarStatus(json: [
            "enabled": true, "accounts": ["me@x.com"], "frequency": "twice-daily", "time": "08:00",
            "last_sync_at": "2026-09-28T08:00:00Z",
        ])
        XCTAssertEqual(calendar.cadenceText, "Twice daily at 08:00")
        XCTAssertNotNil(calendar.lastSyncAt)
        let ambient = ObserveAmbientStatus(json: ["enabled": false, "total_signals": 3])
        let subs = ObserveSubscription.page(from: [
            "items": [
                ["subscription_id": "s1", "display_name": "HN", "enabled": true, "consecutive_failures": 0,
                 "action_id": "web.fetch", "next_run_at_ms": 1_790_000_000_000],
                ["subscription_id": "s2", "display_name": "RSS", "enabled": true, "consecutive_failures": 2,
                 "action_id": "rss.poll"],
            ],
            "total": 5,
        ])
        XCTAssertEqual(subs.total, 5)
        XCTAssertEqual(subs.items.map(\.stateLabel), ["Listening", "Retrying"])
        XCTAssertEqual(subs.items[0].provider, "Web")

        XCTAssertNil(ObserveSourcesCount.enabled(channels: nil, calendar: nil, ambient: nil, enabledSubscriptions: nil))
        // channels (any enabled) 1 + calendar 1 + tabs 0 + 5 subscriptions.
        XCTAssertEqual(
            ObserveSourcesCount.enabled(channels: channels, calendar: calendar, ambient: ambient, enabledSubscriptions: 5),
            7
        )
        XCTAssertEqual(ObserveSourcesCount.enabled(channels: [], calendar: nil, ambient: nil, enabledSubscriptions: nil), 0)

        let catchUp = ObserveCatchUpStatus(json: ["status": [
            "phase": "active", "policy": ["enabled": true], "processed_items": 4, "reserved_items": 1,
            "remaining_items": 20,
        ]])
        XCTAssertEqual(catchUp.phaseLabel, "Active")
        XCTAssertEqual(catchUp.progressLine, "4 processed · 1 running · 20 budget left")
    }

    func testSourceBlocksFailIndependently() async {
        MockURLProtocol.handler = { request in
            let path = request.url!.path
            if path.hasSuffix("/channel-assist/channels") {
                return try Self.json(request, 500, ["error": "gmail token expired"])
            }
            if path.hasSuffix("/observe/calendar/status") { return try Self.json(request, 200, ["enabled": true]) }
            if path.hasSuffix("/observe/subscriptions") {
                XCTAssertTrue(request.url!.query!.contains("enabled=true"))
                XCTAssertTrue(request.url!.query!.contains("limit=20"))
                return try Self.json(request, 200, ["items": [], "total": 2])
            }
            if path.hasSuffix("/ambient/status") { return try Self.json(request, 200, ["enabled": true]) }
            return try Self.json(request, 200, ["status": ["phase": "completed"]])
        }
        let model = ObserveSourcesViewModel(client: ObserveDeckClient(
            session: mockSession(), baseURL: URL(string: "http://test.local")!
        ))
        await model.reloadAll()
        XCTAssertEqual(model.channels.error, "gmail token expired")
        XCTAssertNil(model.channels.value)
        XCTAssertEqual(model.calendar.value?.enabled, true)
        XCTAssertEqual(model.enabledSubscriptionTotal, 2)
        XCTAssertEqual(model.catchUp.value?.phase, "completed")
        // calendar 1 + tabs 1 + 2 subscriptions; the failed channel block adds nothing.
        XCTAssertEqual(model.enabledCount, 4)
    }

    // MARK: recent

    func testRecentListParsingMergesAndCapsNewestFirst() {
        let meetings = RecentCapture.meetings(from: ["recent": [
            ["thread_id": "meeting-a", "session_id": "s", "title": "Standup", "updated_at": 1_790_000_000_000],
            ["thread_id": "meeting-b", "title": NSNull(), "updated_at": 1_790_000_100],   // seconds
            ["title": "no thread"],
        ]])
        XCTAssertEqual(meetings.map(\.threadId), ["meeting-a", "meeting-b"])
        XCTAssertEqual(meetings[1].title, "meeting-b")
        let watch = RecentCapture.watchSessions(from: ["sessions": [
            ["id": "w1", "title": "Figma review", "updated_at": 1_790_000_050_000],
        ]])
        XCTAssertEqual(watch.first?.threadId, "screen-watch")
        let merged = RecentCapture.merged(meetings, watch)
        XCTAssertEqual(merged.map(\.id), ["m:meeting-b", "o:w1", "m:meeting-a"])

        let many = (0..<40).map {
            RecentCapture(id: "\($0)", kind: .meeting, title: "t", threadId: "t\($0)",
                          updatedAt: Date(timeIntervalSince1970: Double($0)), mode: nil)
        }
        let capped = RecentCapture.merged(many)
        XCTAssertEqual(capped.count, RecentCapture.maxRows)
        XCTAssertEqual(capped.first?.id, "39")
    }

    func testRecentViewModelSurvivesAScreenWatchFailure() async {
        MockURLProtocol.handler = { request in
            if request.url!.path.hasSuffix("/chat/sessions") { return try Self.json(request, 500, [:]) }
            return try Self.json(request, 200, ["recent": [["thread_id": "meeting-x", "updated_at": 5]]])
        }
        let model = ObserveRecentViewModel(meetings: MeetingsClient(
            session: mockSession(), baseURL: URL(string: "http://test.local")!
        ))
        await model.reload()
        XCTAssertEqual(model.block.value?.map(\.threadId), ["meeting-x"])
        XCTAssertNil(model.block.error)
    }

    // MARK: audio preferences

    func testAudioPreferenceDecodeAndEncode() throws {
        let catalog = ObserveAudioCatalog.decode(providers: [
            "surface_profiles": [
                "meeting-diarized-v2": ["surface": "meeting", "vad": ["enabled": true],
                                        "recording_stt": ["enabled": true], "diarization": ["enabled": true]],
                "compat-listening-local-v1": ["surface": "listening", "streaming_stt": ["enabled": true]],
                "dictation-fast": ["surface": "dictation"],
            ],
            "default_surface_profiles": ["meeting": "meeting-diarized-v2"],
        ])
        XCTAssertEqual(catalog.profiles[.meeting]?.first?.label, "Meeting Diarized")
        XCTAssertEqual(catalog.profiles[.meeting]?.first?.summary, "Voice activity · Transcription · Speakers")
        XCTAssertEqual(catalog.profiles[.listening]?.first?.label, "Listening Local")
        XCTAssertEqual(catalog.defaults[.meeting], "meeting-diarized-v2")
        XCTAssertNil(catalog.defaults[.listening])

        let sel = ObserveAudioPreferences.selections(from: [
            "surface_profiles": ["meeting": "meeting-diarized-v2", "listening": "default", "dictation": "x"],
        ])
        XCTAssertEqual(sel, [.meeting: "meeting-diarized-v2"])

        let patch = ObserveAudioPreferences.patch(surface: .listening, profileId: nil)
        XCTAssertEqual((patch["surface_profiles"] as? [String: String]), ["listening": ""])
        let clears = (patch["surface_stage_options"] as? [String: [String: String]])?["listening"]
        XCTAssertEqual(clears?.count, 5)
        XCTAssertEqual(Set(clears?.values ?? [:].values), [""])
    }

    func testAudioSaveRollsBackOnFailure() async {
        var puts = 0
        MockURLProtocol.handler = { request in
            if request.httpMethod == "PUT" {
                puts += 1
                return try Self.json(request, 503, ["error": "busy"])
            }
            if request.url!.path.hasSuffix("/media/preferences") {
                return try Self.json(request, 200, ["surface_profiles": ["meeting": "a"]])
            }
            return try Self.json(request, 200, ["surface_profiles": [:]])
        }
        let model = ObserveAudioProfilesViewModel(client: ObserveDeckClient(
            session: mockSession(), baseURL: URL(string: "http://test.local")!
        ))
        await model.reload()
        XCTAssertEqual(model.configuredSurfaces, 2)
        await model.select("b", for: .meeting)
        XCTAssertEqual(puts, 1)
        XCTAssertEqual(model.selections[.meeting], "a", "a failed save must roll back")
        XCTAssertNotNil(model.saveError)
    }

    func testAudioUnavailableShowsDash() async {
        MockURLProtocol.handler = { request in try Self.json(request, 500, [:]) }
        let model = ObserveAudioProfilesViewModel(client: ObserveDeckClient(
            session: mockSession(), baseURL: URL(string: "http://test.local")!
        ))
        await model.reload()
        XCTAssertNil(model.configuredSurfaces)
        XCTAssertNotNil(model.loadError)
    }

    // MARK: bug-fix rules

    func testReattachNeverResumesTheMicForABroadcastArm() {
        let broadcast = ObservationArm(sessionId: "b1", uploadToken: "t", threadId: "th", micEnabled: true)
        let listen = ObservationArm(sessionId: "l1", uploadToken: "t", threadId: "th", micEnabled: false)
        XCTAssertEqual(ObserveCaptureRules.reattach(arm: broadcast, activeSessionIds: ["b1"]), .broadcastLive(sessionId: "b1"))
        XCTAssertEqual(ObserveCaptureRules.reattach(arm: listen, activeSessionIds: ["l1"]), .resumeMic)
        XCTAssertEqual(ObserveCaptureRules.reattach(arm: listen, activeSessionIds: []), .clearStale)
        XCTAssertEqual(ObserveCaptureRules.reattach(arm: nil, activeSessionIds: ["l1"]), .none)
    }

    func testBroadcastArmResetsWhenTheSessionDisappears() async {
        XCTAssertFalse(ObserveCaptureRules.broadcastArmExpired(armedSessionId: nil, activeSessionIds: []))
        XCTAssertFalse(ObserveCaptureRules.broadcastArmExpired(armedSessionId: "b1", activeSessionIds: ["b1"]))
        XCTAssertTrue(ObserveCaptureRules.broadcastArmExpired(armedSessionId: "b1", activeSessionIds: ["x"]))

        var live = true
        MockURLProtocol.handler = { request in
            try Self.json(request, 200, ["active": live ? [["session_id": "b1", "mode": "passive"]] : []])
        }
        let vm = MeetingsViewModel(client: MeetingsClient(session: mockSession(), baseURL: URL(string: "http://test.local")!))
        ObservationArm(sessionId: "b1", uploadToken: "t", threadId: "th", micEnabled: true).save()
        vm.adoptArmedBroadcast(sessionId: "b1")
        await vm.refreshActive()
        XCTAssertEqual(vm.armedBroadcastSessionId, "b1")
        live = false
        await vm.refreshActive()
        XCTAssertNil(vm.armedBroadcastSessionId)
        XCTAssertNil(ObservationArm.claim(), "the ended broadcast's arm is cleared")
    }

    func testMeetingsErrorsSurfaceAndRefreshForcesTheCalendar() async {
        var sawRefresh = false
        MockURLProtocol.handler = { request in
            if request.url!.path.hasSuffix("/meetings/upcoming") {
                sawRefresh = request.url!.query?.contains("refresh=true") == true
                return try Self.json(request, 502, [:])
            }
            return try Self.json(request, 500, [:])
        }
        let vm = MeetingsViewModel(client: MeetingsClient(session: mockSession(), baseURL: URL(string: "http://test.local")!))
        XCTAssertFalse(vm.upcomingLoaded)
        await vm.refresh(force: true)
        XCTAssertTrue(sawRefresh)
        XCTAssertTrue(vm.upcomingLoaded)
        XCTAssertNotNil(vm.upcomingError)
        XCTAssertNotNil(vm.activeError)
        XCTAssertTrue(vm.upcoming.isEmpty)
    }
}
