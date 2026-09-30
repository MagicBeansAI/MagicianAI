import XCTest
import SwiftUI
import UIKit
@testable import Magician

final class ModelAndUtilityTests: XCTestCase {
    func testGeneratedProductIdentityStaysPresentationOnly() {
        XCTAssertEqual(ProductIdentity.productName.trimmingCharacters(in: .whitespacesAndNewlines), ProductIdentity.productName)
        XCTAssertEqual(ProductIdentity.hostAppName.trimmingCharacters(in: .whitespacesAndNewlines), ProductIdentity.hostAppName)
        XCTAssertEqual(ProductIdentity.assistantFallbackName.trimmingCharacters(in: .whitespacesAndNewlines), ProductIdentity.assistantFallbackName)
        XCTAssertFalse(ProductIdentity.productName.isEmpty)
        XCTAssertFalse(ProductIdentity.hostAppName.isEmpty)
        XCTAssertFalse(ProductIdentity.assistantFallbackName.isEmpty)
        XCTAssertFalse(
            [
                ProductIdentity.productName,
                ProductIdentity.hostAppName,
                ProductIdentity.assistantFallbackName
            ]
            .joined(separator: " ")
            .lowercased()
            .contains("magician")
        )
    }

    func testRoadmapContainsOnlyEvidenceBackedPendingCapabilities() {
        XCTAssertEqual(
            RoadmapCatalog.pending.map(\.name),
            [
                "Screenshot + dictation Shortcut / Action Button flow",
                "Inline visual confirmations in Siri",
                "Agent-assisted negotiation from messaging apps",
                "Camera or sketch handoff to VibeDev",
                "Screen-broadcast App Copilot guidance"
            ]
        )

        let shippedOrSuperseded = [
            "Apple Reminders from Worth a look",
            "Dynamic Island Task Tracking",
            "Action Extension (Text Interceptor)",
            "Custom Keyboard (Ghost Writer)",
            "Shadow Meeting (Audio Interception)",
            "Screen Broadcast Extension (ReplayKit)",
            "QR setup for a Magician endpoint"
        ]
        XCTAssertTrue(Set(RoadmapCatalog.pending.map(\.name)).isDisjoint(with: Set(shippedOrSuperseded)))
    }

    func testAtAGlancePrioritizesNeedsYouAndBoundsLockScreenCopy() throws {
        let longTitle = String(repeating: "Important choice ", count: 12)
        let data = try JSONSerialization.data(withJSONObject: [
            "generated_at": 42,
            "counts": ["needs_you": 2, "active_work": 4],
            "sections": [
                "needs_you": [["title": "  \(longTitle)\n  "]],
                "active_work": [[
                    "title": "Build the release",
                    "reason": "Running",
                    "task_id": "task-1"
                ]]
            ]
        ])

        let snapshot = try MagicanGlanceSnapshot.reducingToday(data)

        XCTAssertEqual(snapshot.focus, .needsYou)
        XCTAssertEqual(snapshot.needsYouCount, 2)
        XCTAssertLessThanOrEqual(snapshot.subtitle.count, 96)
        XCTAssertEqual(snapshot.destinationURL.absoluteString, "magican://attention")
    }

    func testAtAGlanceVisibleEqualityIgnoresRefreshTimestamp() {
        let first = MagicanGlanceSnapshot(
            generatedAt: 1,
            focus: .activeWork,
            title: "Build",
            subtitle: "Working",
            needsYouCount: 0,
            activeWorkCount: 1,
            taskID: "task-1"
        )
        let refreshed = MagicanGlanceSnapshot(
            generatedAt: 2,
            focus: .activeWork,
            title: "Build",
            subtitle: "Working",
            needsYouCount: 0,
            activeWorkCount: 1,
            taskID: "task-1"
        )

        XCTAssertTrue(first.hasSameVisibleContent(as: refreshed))
    }

    func testAtAGlanceCacheRejectsAnOlderRacingProjection() throws {
        let suite = "MagicanGlanceCacheTests.\(UUID().uuidString)"
        let store = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { store.removePersistentDomain(forName: suite) }
        let newer = MagicanGlanceSnapshot(
            generatedAt: 200,
            focus: .activeWork,
            title: "New state",
            subtitle: "Working",
            needsYouCount: 0,
            activeWorkCount: 1,
            taskID: "task-new"
        )
        let older = MagicanGlanceSnapshot(
            generatedAt: 199,
            focus: .ready,
            title: "Old state",
            subtitle: "Ready",
            needsYouCount: 0,
            activeWorkCount: 0
        )

        XCTAssertTrue(MagicanGlanceCache.save(newer, store: store))
        XCTAssertFalse(MagicanGlanceCache.save(older, store: store))
        XCTAssertEqual(MagicanGlanceCache.load(store: store), newer)
    }

    func testCircularAtAGlanceWidgetDoesNotHideNeedsYouBehindTalk() throws {
        let root = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
        let source = try String(
            contentsOf: root.appendingPathComponent("MagiosWidgets/MagiosWidgets.swift"),
            encoding: .utf8
        )
        let circular = try XCTUnwrap(
            source.split(separator: "case .accessoryCircular:", maxSplits: 1).last?
                .split(separator: "case .accessoryRectangular:", maxSplits: 1).first
        )

        XCTAssertTrue(circular.contains("if entry.snapshot.focus == .ready"))
        XCTAssertTrue(circular.contains("Button(intent: ArmAmbientIntent())"))
        XCTAssertTrue(circular.contains("Image(systemName: focusIcon)"))
    }

    func testAttentionItemDecodesNestedContractAndDerivesFields() throws {
        let item = try JSONDecoder().decode(AttentionItem.self, from: jsonData([
            "id": "item-1", "title": "Choose a route", "summary": "Need a decision",
            "item_type": "escalation", "status": "open",
            "metadata": [
                "attention_kind": "clarification", "question": "Which route?", "hint": "Pick one",
                "input_schema": [
                    "type": "choice", "placeholder": "Select", "allow_other": true,
                    "confirm_label": "Go", "deny_label": "Stop",
                    "options": [["id": "fast", "label": "Fast", "requires_input": false]]
                ],
                "hitl_request": [
                    "source": "agentic", "input_type": "text",
                    "identifiers": ["correlation_id": "corr-1", "pause_state_id": "pause-1"]
                ]
            ]
        ]))

        XCTAssertEqual(item.source, "agentic")
        XCTAssertEqual(item.correlationId, "corr-1")
        XCTAssertEqual(item.inputType, "choice")
        XCTAssertEqual(item.options.map(\.id), ["fast"])
        XCTAssertEqual(item.prompt, "Which route?")
        XCTAssertEqual(item.hint, "Pick one")
        XCTAssertEqual(item.placeholder, "Select")
        XCTAssertEqual(item.confirmLabel, "Go")
        XCTAssertEqual(item.denyLabel, "Stop")
        XCTAssertTrue(item.isActionable)
    }

    func testAttentionFallbacksAreLenient() throws {
        let approval = try JSONDecoder().decode(AttentionItem.self, from: jsonData([
            "id": "approve-1", "title": "Approve?", "item_type": "approval", "status": "open"
        ]))
        XCTAssertEqual(approval.source, "approval")
        XCTAssertEqual(approval.inputType, "confirmation")
        XCTAssertEqual(approval.correlationId, "approve-1-hitl")
        XCTAssertEqual(approval.placeholder, "Type your response…")

        let malformed = try JSONDecoder().decode(AttentionItem.self, from: jsonData([
            "title": 42, "metadata": "unexpected"
        ]))
        XCTAssertEqual(malformed.title, "Needs attention")
        XCTAssertNil(malformed.metadata)
    }

    func testFeedResponseDefaultsMissingOrMalformedLanesToEmpty() throws {
        let response = try JSONDecoder().decode(FeedAttentionResponse.self, from: jsonData([
            "requests": "bad", "approvals": [], "running": []
        ]))
        XCTAssertTrue(response.requests.isEmpty)
        XCTAssertTrue(response.escalations.isEmpty)
        XCTAssertNil(response.pages)
    }

    func testExecutionPanelAndMessageModelsDecodeSnakeCase() throws {
        let state = try JSONDecoder().decode(ExecutionPanelState.self, from: jsonData([
            "default_tab": "run",
            "overview": [
                "task_id": "task-1", "execution_id": "exec-1", "principal": "local", "workspace": "default",
                "ui_thread_id": "general", "title": "Test", "description": "Desc", "status": "running",
                "assigned_agent_id": "agent", "active_agent_id": "agent", "has_plan": true, "progress": 0.5,
                "current_step": 2, "created_at": 1, "updated_at": 2
            ],
            "run": ["summary": "Working", "recent_activity": [["id": "f1", "kind": "step", "timestamp": 3, "title": "Compile", "agent_id": "agent"]]],
            "output": ["deliveries": []],
            "debug": ["latest_error_message": NSNull(), "history_count": 4]
        ]))
        XCTAssertEqual(state.overview.taskId, "task-1")
        XCTAssertEqual(state.overview.status, "running")
        XCTAssertEqual(state.run.recentActivity.first?.agentId, "agent")
        XCTAssertEqual(state.debug.historyCount, 4)

        let content = try JSONDecoder().decode(ChatMessageContentData.self, from: jsonData([
            "type": "escalation", "pause_state_id": "pause", "correlation_id": "corr",
            "execution_id": "exec-9",
            "content_blocks": [["type": "file", "filename": "a.md", "mime_type": "text/markdown"]]
        ]))
        XCTAssertEqual(content.hitlCorrelationId, "corr")
        XCTAssertEqual(content.contentBlocks?.first?.mimeType, "text/markdown")

        // An escalation card WITHOUT its execution id has no canonical response
        // identity — deliberately. The respond path dispatches escalations to
        // agentic-resume, and the backend recovers a missing execution id by
        // parsing the correlation id's legacy `<exec>:<plan>:<step>` key format;
        // a canonical pause key (`agent:<agent>:…`) parsed that way yields the
        // literal segment `agent` and mis-scopes the resume. Failing closed here
        // ("missing its canonical response identity") beats answering into the
        // wrong execution. This fixture used to omit `execution_id` and expect
        // "corr" — the pre-canonical-HITL contract.
        let unanswerable = try JSONDecoder().decode(ChatMessageContentData.self, from: jsonData([
            "type": "escalation", "pause_state_id": "pause", "correlation_id": "corr"
        ]))
        XCTAssertNil(unanswerable.hitlCorrelationId)
    }

    /// Every status the backend's `TaskStatus` can serialise must survive a delta
    /// decode. `status` used to be a closed Swift enum missing five of these, and
    /// because the overview only ever decodes as a child of the delta, one
    /// unmodelled value threw and discarded the ENTIRE delta — the chat task card
    /// stopped updating and the Live Activity never finished, both in silence.
    /// An unknown status must degrade to a passed-through string, never a throw.
    func testExecutionPanelDeltaDecodesEveryBackendTaskStatus() throws {
        // Mirrors magician/src/magician_v2/storage/task_models.rs `TaskStatus`,
        // plus a not-yet-invented value standing in for the next variant added.
        let statuses = [
            "pending", "planning", "ready", "running", "paused",
            "completed", "failed", "cancelled", "deferred",
            "some_future_status",
        ]

        for status in statuses {
            let payload = jsonData([
                "principal": "local",
                "workspace": "default",
                "task_id": "task-1",
                "execution_id": "exec-1",
                "state": [
                    "default_tab": "run",
                    "overview": [
                        "task_id": "task-1", "execution_id": "exec-1", "principal": "local",
                        "workspace": "default", "ui_thread_id": "general", "title": "Test",
                        "description": "Desc", "status": status, "assigned_agent_id": "agent",
                        "active_agent_id": "agent", "has_plan": true, "progress": 0.5,
                        "current_step": 2, "created_at": 1, "updated_at": 2,
                    ],
                    "run": ["summary": "Working", "recent_activity": []],
                    "output": ["deliveries": []],
                    "debug": ["latest_error_message": NSNull(), "history_count": 4],
                ],
            ])

            let delta = try XCTUnwrap(
                decodeExecutionPanelDelta(from: payload),
                "status '\(status)' discarded the whole ExecutionPanelDelta"
            )
            XCTAssertEqual(delta.state.overview.status, status)
            // The rest of the delta must survive alongside the status.
            XCTAssertEqual(delta.taskId, "task-1")
            XCTAssertEqual(delta.state.run.summary, "Working")
        }
    }

    /// Pinned to the frame the backend actually broadcasts. The previous shape
    /// expected a `lines` array that the wire never carried, so every chunk
    /// died in a `try?` and the transcript's terminal stayed empty for the
    /// feature's whole life.
    func testShellOutputChunkDecodesTheRealWireShape() throws {
        let chunk = try JSONDecoder().decode(ShellOutputChunkEventData.self, from: jsonData([
            "execution_id": "exec-1", "step_id": "step-1", "step_index": 0,
            "command": "cargo build", "stream": "stdout",
            "data": "Compiling magician\n\n   done\n",
            "sequence": 0, "is_final": false, "timestamp": 1,
        ]))
        XCTAssertEqual(chunk.executionId, "exec-1")
        // Interior blank kept (real output); trailing-newline artifact dropped.
        XCTAssertEqual(chunk.lines, ["Compiling magician", "", "   done"])
        XCTAssertNil(chunk.exitCode)

        let final = try JSONDecoder().decode(ShellOutputChunkEventData.self, from: jsonData([
            "execution_id": "exec-1", "step_id": "step-1", "step_index": 0,
            "command": "", "stream": "stdout", "data": "",
            "sequence": 4, "is_final": true, "exit_code": 0, "timestamp": 2,
        ]))
        XCTAssertTrue(final.isFinal)
        XCTAssertEqual(final.exitCode, 0)
        XCTAssertTrue(final.lines.isEmpty)
    }

    /// Only the backend's three terminal states end a Live Activity. `cancelled`
    /// is included (it previously was not, so a cancelled run left the activity
    /// running forever) and an unknown status is never treated as finished.
    func testExecutionPanelOverviewTerminalStatuses() throws {
        func overview(status: String) throws -> ExecutionPanelOverview {
            try JSONDecoder().decode(ExecutionPanelOverview.self, from: jsonData([
                "task_id": "t", "principal": "local", "workspace": "default",
                "ui_thread_id": "general", "title": "T", "description": "D",
                "status": status, "assigned_agent_id": "a", "has_plan": false,
                "created_at": 1, "updated_at": 2,
            ]))
        }

        for status in ["completed", "failed", "cancelled"] {
            XCTAssertTrue(try overview(status: status).isTerminal, "\(status) should be terminal")
        }
        for status in ["pending", "planning", "ready", "running", "paused", "deferred", "unknown"] {
            XCTAssertFalse(try overview(status: status).isTerminal, "\(status) should not be terminal")
        }
    }

    func testThreadAndSharedModelsRoundTrip() throws {
        let thread = try JSONDecoder().decode(UiThreadRecord.self, from: jsonData([
            "principal": "local", "workspace": "default", "id": "general", "name": "General", "archived": false,
            "sort_order": 0, "memory_summary": "Summary", "memory_updated_at": 5, "created_at": 1, "updated_at": 2
        ]))
        XCTAssertEqual(thread.sortOrder, 0)
        XCTAssertEqual(thread.memoryUpdatedAt, 5)

        let original = SharedItem(id: "shared-1", kind: .image, text: nil, storedName: "blob", filename: "photo.jpg", mime: "image/jpeg")
        let decoded = try JSONDecoder().decode(SharedItem.self, from: JSONEncoder().encode(original))
        XCTAssertEqual(decoded, original)
    }

    func testSpeechTagExtractionAndStripping() {
        let tagged = "Visible <speech voice=\"x\">  Read   this </speech> hidden <SPEECH>and this</SPEECH>"
        XCTAssertTrue(SpeechTags.hasSpeechTags(tagged))
        XCTAssertEqual(SpeechTags.spokenText(tagged), "Read this and this")
        XCTAssertEqual(SpeechTags.stripped(tagged), "Visible   Read   this  hidden and this")
        XCTAssertEqual(SpeechTags.spokenText("  Whole reply  \n"), "Whole reply")
        XCTAssertFalse(SpeechTags.hasSpeechTags("<speech></speech>"))
    }

    func testMarkdownStrippingKeepsReadableProse() {
        let input = "# Title\n- **Bold** and [link](https://example.com) with `code`\n```swift\nlet hidden = true\n```"
        let result = SpeechSynthesizer.stripMarkdown(input)
        XCTAssertTrue(result.contains("Title"))
        XCTAssertTrue(result.contains("Bold and link with code"))
        XCTAssertFalse(result.contains("hidden"))
        XCTAssertFalse(result.contains("**"))
    }

    func testArtifactRoutingCoversSupportedTypes() {
        let cases: [(String?, String, ArtifactKind)] = [
            ("image/png", "x.bin", .image), (nil, "x.pdf", .pdf), ("text/html", "x", .html),
            ("video/mp4", "x", .video), (nil, "x.m4a", .audio), (nil, "README.md", .text(.markdown)),
            ("application/json", "x", .text(.json)), ("text/csv", "x", .text(.plain)), (nil, "x.zip", .other)
        ]
        for (mime, filename, expected) in cases {
            XCTAssertEqual(ArtifactKind.from(mime: mime, filename: filename), expected)
        }
        XCTAssertEqual(ArtifactKind.from(mime: nil, filename: "x.json").label, "JSON")
        XCTAssertEqual(ArtifactKind.from(mime: nil, filename: "x.pdf").systemIcon, "doc.richtext")
    }

    func testArtifactFactoriesResolvePathsAndNames() {
        let output = ArtifactRef.taskOutput(taskId: "task-1", relativePath: "reports/final report.md", mime: "text/markdown")
        XCTAssertEqual(output.filename, "final report.md")
        XCTAssertTrue(output.resolvedURL.contains("final%20report.md"))
        XCTAssertEqual(output.kind, .text(.markdown))

        let configuredOrigin = URL(string: "https://ios.example.com")!
        XCTAssertEqual(ArtifactRef.direct(
            url: "/api/file.pdf",
            mime: nil,
            name: nil,
            baseURL: configuredOrigin
        ).resolvedURL,
                       "https://ios.example.com/api/file.pdf")
        XCTAssertEqual(ArtifactRef.direct(url: "https://example.com/a.png", mime: nil, name: "preview").filename,
                       "preview")

        let sessionOutput = ArtifactRef.sessionOutput(
            sessionId: "session-1",
            relativePath: "outputs/reports/final report.pdf",
            mime: "application/pdf"
        )
        XCTAssertEqual(sessionOutput.filename, "final report.pdf")
        XCTAssertTrue(sessionOutput.resolvedURL.contains("/chat/sessions/session-1/outputs/reports/"))
        XCTAssertFalse(sessionOutput.resolvedURL.contains("/outputs/outputs/"))
        XCTAssertTrue(sessionOutput.resolvedURL.contains("final%20report.pdf"))
    }

    func testAuthenticatedWebViewSchemeRoundTrip() {
        let https = URL(string: "https://ios.example.com/api/file?q=1")!
        let custom = AuthWebView.toSchemeURL(https)
        XCTAssertEqual(custom?.scheme, "magartifact")
        XCTAssertEqual(AuthWebView.toHttpsURL(custom!), https)
        XCTAssertEqual(AuthWebView.toSchemeURL(URL(string: "http://example.com")!)?.absoluteString,
                       "magartifact://example.com")
    }

    func testAudioEnumsDescribeRouting() {
        XCTAssertTrue(STTSource.auto.prefersOnDevice)
        XCTAssertTrue(STTSource.auto.allowsCloud)
        XCTAssertFalse(STTSource.cloud.prefersOnDevice)
        XCTAssertFalse(STTSource.onDevice.allowsCloud)
        XCTAssertEqual(TTSEngine.magician.label, "Backend host")
    }

    func testThemeCatalogAppliesLightAndDarkPalettes() {
        let prior = UserDefaults.standard.string(forKey: "magican-theme")
        defer {
            if let prior { UserDefaults.standard.set(prior, forKey: "magican-theme") }
            else { UserDefaults.standard.removeObject(forKey: "magican-theme") }
        }
        let theme = ThemeManager()
        XCTAssertEqual(theme.availableThemeFamilies.count, 11)
        XCTAssertEqual(Set(theme.availableThemeFamilies.map(\.lightTheme)).count, 11)
        XCTAssertEqual(Set(theme.availableThemeFamilies.map(\.darkTheme)).count, 11)
        XCTAssertEqual(theme.availableThemes.count, 22)

        for family in theme.availableThemeFamilies {
            let revisionBeforeLight = theme.themeRevision
            theme.applyTheme(family.lightTheme)
            XCTAssertEqual(theme.themeRevision, revisionBeforeLight + 1)
            XCTAssertFalse(theme.isDark, "\(family.name) light palette should be light")
            XCTAssertEqual(theme.colorScheme, .light)
            XCTAssertEqual(theme.currentThemeFamilyID, family.lightTheme)

            let revisionBeforeDark = theme.themeRevision
            theme.setDarkMode(true)
            XCTAssertEqual(theme.themeRevision, revisionBeforeDark + 1)
            XCTAssertTrue(theme.isDark, "\(family.name) dark palette should be dark")
            XCTAssertEqual(theme.colorScheme, .dark)
            XCTAssertEqual(theme.currentThemeName, family.darkTheme)
            XCTAssertEqual(theme.currentThemeFamilyID, family.lightTheme)

            theme.setDarkMode(false)
            XCTAssertEqual(theme.currentThemeName, family.lightTheme)
        }

        theme.applyTheme("longhand-dark")
        theme.applyThemeFamily("arcane-terminal-light", systemDark: true)
        XCTAssertEqual(theme.currentThemeName, "arcane-terminal")
    }

    func testThemeCatalogMirrorsEveryWebFontRoleAndBundlesNewFaces() throws {
        let theme = ThemeManager()
        XCTAssertEqual(Set(ThemeManager.themeFonts.keys), Set(theme.availableThemes))
        XCTAssertTrue(ThemeManager.themeFonts.values.allSatisfy { $0.brand == "Outfit" })
        XCTAssertEqual(
            ThemeManager.fonts(for: "longhand"),
            ThemeFontNames(brand: "Outfit", display: "Outfit", body: "Manrope", mono: "Geist Mono")
        )
        XCTAssertEqual(ThemeManager.fonts(for: "arcane-terminal").body, "IBM Plex Mono")
        XCTAssertEqual(ThemeManager.fonts(for: "retro-16bit").body, "JetBrains Mono")
        XCTAssertEqual(ThemeManager.fonts(for: "mario-8bit").body, "Pixelify Sans")
        XCTAssertEqual(ThemeManager.fonts(for: "risograph").display, "Bricolage Grotesque")
        XCTAssertEqual(ThemeManager.fonts(for: "mixtape").display, "Permanent Marker")
        XCTAssertEqual(ThemeManager.fonts(for: "cartoon").display, "Lilita One")
        XCTAssertEqual(ThemeManager.fonts(for: "jarvis").body, "Manrope")

        let resources = [
            "Outfit": "Outfit", "Manrope": "Manrope", "Geist Mono": "GeistMono",
            "Quicksand": "Quicksand", "Fredoka": "Fredoka",
            "JetBrains Mono": "JetBrainsMono", "Space Grotesk": "SpaceGrotesk",
            "IBM Plex Mono": "IBMPlexMono-Regular", "Fira Code": "FiraCode",
            "Press Start 2P": "PressStart2P-Regular", "Pixelify Sans": "PixelifySans",
            "Bricolage Grotesque": "BricolageGrotesque",
            "Special Elite": "SpecialElite-Regular",
            "Permanent Marker": "PermanentMarker-Regular", "Inter": "Inter",
            "Lilita One": "LilitaOne-Regular", "Rajdhani": "Rajdhani-Regular"
        ]
        let usedFamilies = Set(ThemeManager.themeFonts.values.flatMap {
            [$0.brand, $0.display, $0.body, $0.mono]
        })
        XCTAssertEqual(usedFamilies, Set(resources.keys))
        for family in usedFamilies {
            let resource = try XCTUnwrap(resources[family])
            XCTAssertNotNil(Bundle.main.url(forResource: resource, withExtension: "ttf"), family)
        }
    }

    func testForcedColorSchemeDoesNotLockSystemModeToDark() {
        let priorTheme = UserDefaults.standard.string(forKey: "magican-theme")
        let priorMode = UserDefaults.standard.string(forKey: "magican-appearance-mode")
        defer {
            if let priorTheme { UserDefaults.standard.set(priorTheme, forKey: "magican-theme") }
            else { UserDefaults.standard.removeObject(forKey: "magican-theme") }
            if let priorMode { UserDefaults.standard.set(priorMode, forKey: "magican-appearance-mode") }
            else { UserDefaults.standard.removeObject(forKey: "magican-appearance-mode") }
        }
        let theme = ThemeManager()

        // A dark theme is active (as after a relaunch that persisted the dark variant).
        theme.applyTheme("longhand-dark")
        XCTAssertTrue(theme.isDark)

        // In `.system` mode we must force NOTHING at the presentation boundary, so the
        // window trait (which `systemIsDark` reads back) follows the device rather than
        // being pinned dark. Forcing here was the bug that locked the app to dark.
        theme.appearanceMode = .system
        XCTAssertNil(theme.forcedColorScheme, "system mode must not force a color scheme")

        // Device is light -> reconciliation flips the family to its light variant.
        theme.systemAppearanceChanged(dark: false)
        XCTAssertEqual(theme.currentThemeName, "longhand")
        XCTAssertFalse(theme.isDark)
        XCTAssertNil(theme.forcedColorScheme)

        // Device is dark -> follow it, still without a forced override.
        theme.systemAppearanceChanged(dark: true)
        XCTAssertEqual(theme.currentThemeName, "longhand-dark")
        XCTAssertNil(theme.forcedColorScheme)

        // Forced day/night DO pin the scheme.
        theme.setAppearanceMode(.night, systemDark: false)
        XCTAssertEqual(theme.forcedColorScheme, .dark)
        theme.setAppearanceMode(.day, systemDark: true)
        XCTAssertEqual(theme.forcedColorScheme, .light)
    }

    func testThemeSemanticColorsKeepAccentContentLegible() {
        func rgba(_ color: Color) -> (CGFloat, CGFloat, CGFloat, CGFloat) {
            var red: CGFloat = 0
            var green: CGFloat = 0
            var blue: CGFloat = 0
            var alpha: CGFloat = 0
            XCTAssertTrue(UIColor(color).getRed(&red, green: &green, blue: &blue, alpha: &alpha))
            return (red, green, blue, alpha)
        }
        func assertSameColor(_ lhs: Color, _ rhs: Color) {
            let left = rgba(lhs)
            let right = rgba(rhs)
            XCTAssertEqual(left.0, right.0, accuracy: 0.001)
            XCTAssertEqual(left.1, right.1, accuracy: 0.001)
            XCTAssertEqual(left.2, right.2, accuracy: 0.001)
            XCTAssertEqual(left.3, right.3, accuracy: 0.001)
        }

        let theme = ThemeManager()

        let webAccentForegrounds = [
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
        let webSoftBackgrounds: [String: (String, Double)] = [
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
        XCTAssertEqual(Set(webAccentForegrounds.keys), Set(theme.availableThemes))
        XCTAssertEqual(Set(webSoftBackgrounds.keys), Set(theme.availableThemes))
        for themeName in theme.availableThemes {
            theme.applyTheme(themeName)
            assertSameColor(
                theme.onAccentColor,
                Color(hex: try! XCTUnwrap(webAccentForegrounds[themeName]))
            )
            let soft = try! XCTUnwrap(webSoftBackgrounds[themeName])
            assertSameColor(
                theme.softBackgroundColor,
                Color(hex: soft.0).opacity(soft.1)
            )
        }

        theme.applyTheme("mono-dark")
        assertSameColor(theme.cardColor, theme.elevatedColor)

        theme.applyTheme("longhand")
        let lightWarning = rgba(theme.warningColor)

        theme.applyTheme("soft-machine")
        assertSameColor(theme.onAccentColor, Color.white)

        theme.applyTheme("longhand-dark")
        let darkWarning = rgba(theme.warningColor)
        XCTAssertNotEqual(lightWarning.0, darkWarning.0)
        XCTAssertGreaterThan(rgba(theme.cardBorderColor).3, 0)
    }

    func testThemePickerPreviewUsesTheVariantThatSelectionWillApply() {
        let theme = ThemeManager()
        let family = theme.availableThemeFamilies[0]

        theme.appearanceMode = .day
        XCTAssertEqual(
            UIColor(theme.previewPalette(for: family, systemDark: true).background),
            UIColor(ThemeManager.paletteColors(for: family.lightTheme).background)
        )

        theme.appearanceMode = .night
        XCTAssertEqual(
            UIColor(theme.previewPalette(for: family, systemDark: false).background),
            UIColor(ThemeManager.paletteColors(for: family.darkTheme).background)
        )

        theme.appearanceMode = .system
        XCTAssertEqual(
            UIColor(theme.previewPalette(for: family, systemDark: false).accent),
            UIColor(ThemeManager.paletteColors(for: family.lightTheme).accent)
        )
        XCTAssertEqual(
            UIColor(theme.previewPalette(for: family, systemDark: true).accent),
            UIColor(ThemeManager.paletteColors(for: family.darkTheme).accent)
        )
    }

    func testStagedAttachmentAndChatMessageDerivedBehavior() {
        let image = StagedAttachment(id: UUID(), remoteId: nil, filename: "photo.jpg", mime: "image/jpeg", thumbnail: Data(), uploading: true)
        XCTAssertTrue(image.isImage)
        let file = StagedAttachment(id: UUID(), remoteId: nil, filename: "doc.pdf", mime: "application/pdf", thumbnail: nil, uploading: false)
        XCTAssertFalse(file.isImage)

        let id = "same"
        XCTAssertEqual(ChatMessage(id: id, isUser: true, text: "a", type: .text),
                       ChatMessage(id: id, isUser: false, text: "a", type: .system("ignored by equality")))
    }

    func testThinkingMapUITestFallbackIsPresentedAsDemoRatherThanOffline() {
        let fallback = ThinkingMapFrontierFallback.demo

        XCTAssertEqual(fallback.statusLabel, "DEMO")
        XCTAssertEqual(fallback.paletteLabel, "DEMO STARTERS")
        XCTAssertEqual(fallback.cardLabel, "DEMO STARTER")
        XCTAssertNil(fallback.message)
        XCTAssertFalse(fallback.canRetry)
        XCTAssertFalse(fallback.provenance.localizedCaseInsensitiveContains("offline"))
    }

    func testThinkingMapFrontierFailuresPreserveUsefulReasonAndPermitRetry() {
        let timedOut = ThinkingMapFrontierFallback.classify(ContextualAssistClientError.timedOut)
        let authentication = ThinkingMapFrontierFallback.classify(
            ContextualAssistClientError.authentication("Secure access setup is incomplete.")
        )

        XCTAssertEqual(timedOut.statusLabel, "AI UNAVAILABLE")
        XCTAssertEqual(timedOut.message, "The facilitator took too long to read this map. Try again.")
        XCTAssertTrue(timedOut.canRetry)
        XCTAssertEqual(authentication.message, "Secure access setup is incomplete.")
        XCTAssertTrue(authentication.canRetry)
    }

    func testAudioNotePaginationResetsForNewQueriesAndRepairsOffsetAfterDelete() {
        var state = AudioNotePaginationState()
        let first = state.request(reset: true, query: "  Launch  ")
        XCTAssertEqual(first.offset, 0)
        XCTAssertEqual(first.query, "Launch")
        XCTAssertTrue(first.resetsItems)
        state.accept(first, pageOffset: 0, pageCount: 20)

        let next = state.request(reset: false, query: "launch")
        XCTAssertEqual(next.offset, 20)
        XCTAssertFalse(next.resetsItems)

        state.removeLoadedItem()
        XCTAssertEqual(state.request(reset: false, query: "LAUNCH").offset, 19)

        let changedQuery = state.request(reset: false, query: "design")
        XCTAssertEqual(changedQuery.offset, 0)
        XCTAssertTrue(changedQuery.resetsItems)
    }
}

final class ObservationUploadPumpStopTests: XCTestCase {
    func testStopRejectsLaterAudioWithoutStartingAnUpload() async {
        let pump = ObservationUploadPump(
            client: ObservationUplinkClient(session: makeMockSession()),
            sessionId: "listen-stop-test",
            token: "token"
        )

        await pump.stop()
        await pump.submit(Data([0, 1, 2, 3]))
        let snapshot = await pump.snapshot()

        XCTAssertTrue(snapshot.ended)
        XCTAssertEqual(snapshot.nextSeq, 0)
        XCTAssertEqual(snapshot.dropped, 0)
    }
}
