import SwiftUI
import UIKit
import XCTest
@testable import Magician

/// A preference alignment hold on iOS (P6.2): the shared projection, the
/// owner-private read, and what each answer posts.
final class AlignmentAskTests: XCTestCase {
    private let projection: [String: Any] = [
        "schema_version": "alignment_ask.v1",
        "kind": "preference_alignment",
        "ask_id": "ask-1",
        "purpose": "allow_once",
        "choices": [
            ["id": "allow_once", "label": "Allow this once"],
            ["id": "change_preference", "label": "Change my preference"],
            ["id": "cancel", "label": "Cancel"],
        ],
        "free_text": true,
        "deadline_at_ms": 1_000_000,
        "stage": "decide",
    ]

    private func row(_ schema: [String: Any]) throws -> AttentionItem {
        try JSONDecoder().decode(
            AttentionItem.self,
            from: JSONSerialization.data(withJSONObject: [
                "id": "a", "title": "t", "item_type": "agentic", "status": "pending",
                "metadata": ["input_schema": schema],
            ])
        )
    }

    func testAHoldIsReadOffTheSchemaAndCarriesNoReason() throws {
        let item = try row([
            "type": "confirmation",
            "escalation_trigger": "preference_alignment",
            "confirm_label": "Allow this once",
            "deny_label": "Cancel",
            "alignment": projection,
        ])
        let hold = try XCTUnwrap(item.alignment)
        XCTAssertEqual(hold.askId, "ask-1")
        // Lasting choices need the private readback; the act's own are shown.
        XCTAssertEqual(hold.actChoices.map(\.id), ["allow_once", "cancel"])
        XCTAssertFalse(hold.isWordingStage)
        XCTAssertEqual(item.inputType, "confirmation")
    }

    func testAMalformedBlockDoesNotBreakTheRow() throws {
        let item = try row([
            "type": "confirmation",
            "alignment": ["kind": "preference_alignment", "choices": "not-a-list"],
        ])
        XCTAssertNil(item.alignment)
        XCTAssertEqual(item.confirmLabel, "Approve")
    }

    func testThePrivateReadParsesTheReasonAndTheReadback() throws {
        let body: [String: Any] = [
            "private": true,
            "retry_reason": NSNull(),
            "alignment": projection.merging([
                "stage": "confirm_wording",
                "explanation": "You asked to keep dinners vegetarian.",
                "act_summary": "swiggy order",
                "intentions": [[
                    "id": "pref-1", "statement": "diet: vegetarian dinners",
                    "assertion_status": "direct_owner_statement", "editable": true,
                ]],
                "choices": [
                    ["id": "confirm_wording", "label": "Save this wording", "lasting": true],
                    ["id": "keep_as_is", "label": "Don't change it"],
                    ["id": "cancel", "label": "Cancel"],
                ],
                "readback": [
                    "kind": "replace", "wording": "vegetarian on weekdays",
                    "superseded": "diet: vegetarian dinners", "token": "tok-123",
                ],
            ]) { _, new in new },
        ]
        let detail = try JSONDecoder().decode(
            AlignmentDetail.self, from: JSONSerialization.data(withJSONObject: body))
        XCTAssertTrue(detail.isPrivate)
        XCTAssertTrue(detail.alignment.isWordingStage)
        XCTAssertEqual(detail.alignment.explanation, "You asked to keep dinners vegetarian.")
        XCTAssertEqual(detail.alignment.intentions.first?.sourceLabel, "You said")
        XCTAssertEqual(detail.alignment.readback?.token, "tok-123")
        XCTAssertEqual(detail.alignment.lastingChoices.map(\.id), ["confirm_wording"])
        XCTAssertEqual(detail.alignment.actChoices.map(\.id), ["cancel"])
    }

    func testACombinedAskListsTheApprovalItAlsoSettles() throws {
        let body: [String: Any] = [
            "private": true,
            "alignment": projection.merging([
                "also_covers": [
                    ["gate": "approval_rule", "reason": "Action matched approval rule"],
                    ["gate": "criticality"],
                ],
            ]) { _, new in new },
        ]
        let detail = try JSONDecoder().decode(
            AlignmentDetail.self, from: JSONSerialization.data(withJSONObject: body))
        XCTAssertEqual(detail.alignment.alsoCovers.map(\.line), [
            "Action matched approval rule", "It is a high-impact action.",
        ])
    }

    func testNotesListOnlyOpenOnesAndRememberNeedsItsToken() throws {
        let body: [String: Any] = [
            "private": true, "open": 1,
            "notices": [
                [
                    "id": "n-1", "created_at_ms": 1, "act_summary": "swiggy order",
                    "explanation": "You usually order mild food.", "intentions": [],
                    "remember": ["statement": "spice: mild", "wording": "spice: mild", "token": "tok-9"],
                    "state": "open",
                ],
                ["id": "n-2", "state": "dismissed"],
                ["state": "open"],
            ],
        ]
        let notices = PreferenceNotices.parse(try JSONSerialization.data(withJSONObject: body))
        XCTAssertEqual(notices.map(\.id), ["n-1"])
        XCTAssertEqual(notices.first?.remember?.token, "tok-9")
        XCTAssertEqual(
            PreferenceNotices.answerBody(remember: true, token: "tok-9") as? [String: String],
            ["answer": "remember", "token": "tok-9"]
        )
        XCTAssertNil(PreferenceNotices.answerBody(remember: true, token: nil))
        XCTAssertEqual(
            PreferenceNotices.answerBody(remember: false, token: nil) as? [String: String],
            ["answer": "dismiss"]
        )
        XCTAssertEqual(
            PreferenceNotices.listURL(base: "https://h/api/magician/v2")?.absoluteString,
            "https://h/api/magician/v2/preference-alignment/notices?surface=ios"
        )
    }

    func testEveryAnswerIsATypedChoiceOrTheOwnersWords() {
        XCTAssertEqual(
            AlignmentAsk.answerValue(choiceId: "allow_once") as? [String: String],
            ["type": "choice", "selected_id": "allow_once"])
        XCTAssertEqual(
            AlignmentAsk.answerValue(choiceId: "confirm_wording", input: "tok-123") as? [String: String],
            ["type": "choice", "selected_id": "confirm_wording", "other_value": "tok-123"])
        XCTAssertEqual(
            AlignmentAsk.answerValue(choiceId: "respond", input: "  order the paneer one ") as? [String: String],
            ["type": "text", "value": "order the paneer one"])
        XCTAssertNil(AlignmentAsk.answerValue(choiceId: "respond", input: "   "))
    }

    func testThePrivateReadNamesThisSurface() {
        let url = AlignmentAsk.detailURL(base: "https://m.example/api/magician/v2",
                                         correlationId: "exec-1:plan:step~ask-1")
        XCTAssertEqual(url?.absoluteString,
                       "https://m.example/api/magician/v2/hitl/exec-1:plan:step~ask-1/alignment?surface=ios")
    }

    func testTheDeadlineSaysALateAnswerRunsNothing() {
        let now = Date(timeIntervalSince1970: 0)
        XCTAssertNil(AlignmentAsk.deadlineLine(deadlineAtMs: nil, now: now))
        XCTAssertEqual(AlignmentAsk.deadlineLine(deadlineAtMs: 600_000, now: now),
                       "Answer within 10 min; after that it won't run.")
        XCTAssertEqual(AlignmentAsk.deadlineLine(deadlineAtMs: 0, now: Date(timeIntervalSince1970: 1)),
                       "The answer window has closed; a late answer runs nothing.")
    }

    // MARK: Your preferences

    private func json(_ object: Any) throws -> Data {
        try JSONSerialization.data(withJSONObject: object)
    }

    func testThePreferencesViewReadsWhatChecksSeeAndSkipsMalformedRows() throws {
        let body = try json([
            "intentions": [
                [
                    "id": "codes", "revision": "r1", "kind": "standing_instruction",
                    "assertion_status": "direct_owner_statement",
                    "statement": "codes: Ask me before giving out door codes",
                    "lifecycle": "active", "editable": true, "last_checked_at_ms": 5,
                ],
                ["kind": "taste"],
            ],
        ])
        let preferences = OwnerPreferences.parse(body)
        XCTAssertEqual(preferences.count, 1)
        XCTAssertEqual(preferences[0].statusWords, "You said it")
        XCTAssertEqual(preferences[0].wording, "Ask me before giving out door codes")
        XCTAssertTrue(preferences[0].editable)
    }

    func testAForgottenEntryKeepsItsDateButNotItsWording() throws {
        let body = try json([
            "entries": [
                ["at_ms": 2, "op": "edited", "actor": ["via": "owner_surface", "surface": "ios"], "wording_erased": true],
            ],
        ])
        let entries = OwnerPreferences.parseHistory(body)
        XCTAssertEqual(entries.count, 1)
        XCTAssertTrue(entries[0].wordingErased)
        XCTAssertNil(entries[0].wordingAfter)
        XCTAssertEqual(entries[0].surface, "ios")
    }

    func testEditsNameTheRevisionOnScreenAndAnEmptyEditIsNotSent() {
        XCTAssertNil(OwnerPreferences.patchBody(action: "edit", expectedRevision: "r1", wording: "  "))
        let edit = OwnerPreferences.patchBody(action: "edit", expectedRevision: "r1", wording: "No red meat")
        XCTAssertEqual(edit?["expected_revision"] as? String, "r1")
        XCTAssertEqual(edit?["wording"] as? String, "No red meat")
        let retire = OwnerPreferences.patchBody(action: "retire", expectedRevision: "r1")
        XCTAssertNil(retire?["wording"])
        XCTAssertNil(OwnerPreferences.addBody(kind: "goal", wording: " ", label: nil, conditions: nil))
    }

    func testForgetCarriesTheRevisionAndAStaleAnswerIsRecognised() throws {
        let url = OwnerPreferences.itemURL(base: "https://h/api/magician/v2", id: "a/b", expectedRevision: "r1")
        XCTAssertEqual(
            url?.absoluteString,
            "https://h/api/magician/v2/preference-alignment/intentions/a%2Fb?surface=ios&expected_revision=r1"
        )
        let stale = try json(["error": "stale_preference", "current_revision": "r2"])
        XCTAssertEqual(OwnerPreferences.outcome(status: 409, data: stale), .stale)
        XCTAssertEqual(OwnerPreferences.outcome(status: 200, data: nil), .applied)
        XCTAssertEqual(OwnerPreferences.outcome(status: 403, data: nil), .failed)
    }
}

// MARK: - Structured preferences and the preferences screen

final class PresentedStatementTests: XCTestCase {
    func testAStoredKeyAndJsonObjectReadsAsATitleAndSortedFields() {
        let presented = PresentedStatement.parse(
            #"output_format_preference: {"media_type":"text/markdown","scope":"both","depth":2}"#
        )
        XCTAssertEqual(presented?.title, "Output format preference")
        XCTAssertEqual(presented?.fields, [
            .init(label: "Depth", value: "2"),
            .init(label: "Media type", value: "text/markdown"),
            .init(label: "Scope", value: "both"),
        ])
        XCTAssertEqual(PresentedStatement.parse(#"ui_test_fallback: {"api_url":"x"}"#)?.title, "UI test fallback")
    }

    func testProseArraysAndBrokenJsonStayAsTheyAre() {
        XCTAssertNil(PresentedStatement.parse("Check with me before ordering meat."))
        XCTAssertNil(PresentedStatement.parse("Note: {not json}"))
        XCTAssertNil(PresentedStatement.parse("list: [1,2]"))
    }
}

/// Renders Settings → Your preferences offscreen from fixtures, in Sorbet
/// light and dark. PNGs land in `PREFERENCES_SHOTS_DIR` (or the temp dir).
@MainActor
final class OwnerPreferencesRenderTests: XCTestCase {
    private var savedTheme = ""

    override func setUp() async throws {
        savedTheme = ThemeManager.shared.currentThemeName
    }

    override func tearDown() async throws {
        ThemeManager.shared.applyTheme(savedTheme)
    }

    private let fixture = Data(#"""
    {"intentions":[
      {"id":"p1","revision":"r1","kind":"standing_instruction","assertion_status":"direct_owner_statement",
       "statement":"Check with me before ordering meat for me.","lifecycle":"active","editable":true},
      {"id":"p2","revision":"r2","kind":"goal","assertion_status":"direct_owner_statement",
       "statement":"Keep dining out under $300 this month.","conditions":"until 31 October","lifecycle":"active","editable":true},
      {"id":"p3","revision":"r3","kind":"taste","assertion_status":"inferred",
       "statement":"output_format_preference: {\"media_type\":\"text/markdown\",\"scope\":\"both execution output and user-facing output\"}",
       "lifecycle":"active","editable":true},
      {"id":"p4","revision":"r4","kind":"taste","assertion_status":"confirmed",
       "statement":"I prefer aisle seats.","lifecycle":"active","editable":true}]}
    """#.utf8)

    func testRenderOwnerPreferencesSorbetLightAndDark() throws {
        let preferences = OwnerPreferences.parse(fixture)
        XCTAssertEqual(preferences.count, 4)
        for theme in ["sorbet", "sorbet-dark"] {
            ThemeManager.shared.applyTheme(theme)
            RunLoop.main.run(until: Date().addingTimeInterval(0.2))
            try render(
                NavigationStack { OwnerPreferencesView(seededWith: preferences) },
                name: "owner-preferences-\(theme)"
            )
        }
    }

    private var outputDir: URL {
        let path = ProcessInfo.processInfo.environment["PREFERENCES_SHOTS_DIR"]
            ?? FileManager.default.temporaryDirectory.appendingPathComponent("preferences-shots").path
        let url = URL(fileURLWithPath: path)
        try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    private func render<V: View>(_ view: V, name: String, width: CGFloat = 393, height: CGFloat = 852) throws {
        let host = UIHostingController(rootView: view.frame(width: width, height: height))
        let window: UIWindow
        if let scene = UIApplication.shared.connectedScenes.compactMap({ $0 as? UIWindowScene }).first {
            window = UIWindow(windowScene: scene)
        } else {
            window = UIWindow()
        }
        window.frame = CGRect(origin: .zero, size: CGSize(width: width, height: height))
        window.overrideUserInterfaceStyle = ThemeManager.shared.isDark ? .dark : .light
        window.rootViewController = host
        window.isHidden = false
        host.view.frame = window.bounds
        host.view.layoutIfNeeded()
        RunLoop.main.run(until: Date().addingTimeInterval(0.8))
        host.view.layoutIfNeeded()
        let format = UIGraphicsImageRendererFormat()
        format.scale = 3
        let image = UIGraphicsImageRenderer(bounds: window.bounds, format: format).image { _ in
            _ = window.drawHierarchy(in: window.bounds, afterScreenUpdates: true)
        }
        window.isHidden = true
        let url = outputDir.appendingPathComponent("\(name).png")
        try XCTUnwrap(image.pngData()).write(to: url)
        print("preferences render: \(url.path)")
    }
}
