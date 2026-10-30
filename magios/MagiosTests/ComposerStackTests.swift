import SwiftUI
import UIKit
import XCTest
@testable import Magician

/// The composer stack's pure rules: the queue row title, the auto-send
/// countdown, presence, the ask dock's items / scoping / switcher, and the
/// planner dock's selection.
final class ComposerStackTests: XCTestCase {
    @MainActor func testPrivateSignInPanelRendersWithoutAnEnvironmentTheme() {
        // Composer sheets use the shared theme rather than an injected
        // EnvironmentObject. Rendering this panel must not trap on presentation.
        let launch = BrowserSignInLaunch(baseURL: URL(string: "https://example.invalid")!,
            headers: [:], ticket: "", authorizationURL: nil, current: { false })
        let host = UIHostingController(rootView: BrowserSignInPanel(launch: launch))
        let size = host.sizeThatFits(in: CGSize(width: 320, height: 720))
        XCTAssertGreaterThan(size.height, 0)
        XCTAssertLessThanOrEqual(size.width, 320)
    }

    func testTranscriptApprovalRequiresTheReviewSurfaceBeforeFeedHydration() throws {
        let content = try JSONDecoder().decode(ChatMessageContentData.self, from: Data(#"{"type":"escalation","input_type":"confirmation","question":"Apply?","execution_id":"run-preview","escalation_type":"approval","pause_state_id":"preview","input_schema":{"type":"confirmation","approval_preview":{"title":"Review the change"}},"options":[{"id":"approve","label":"Approve","action":{"type":"respond_confirmation","confirmed":true}}]}"#.utf8))
        XCTAssertTrue(content.inputSchema?.requiresReview == true)
        let ask = try XCTUnwrap(ComposerAskModel.item(chat: content))
        guard case .handoff = ask.form else { return XCTFail("Do not consent without the review") }
        let choice = ComposerAskChoice(id: "approve", label: "Approve", action: .confirm(true))
        XCTAssertNil(ComposerAskModel.submission(for: content, reply: .choice(choice)))
    }

    @MainActor func testLongChoiceListsStayWithinTheComposerCard() {
        let choices = (1...30).map { ComposerAskChoice(id: "\($0)", label: "Choice \($0): a longer option that must wrap on a phone", action: .confirm(true)) }
        let base = Self.attention(#"{"id":"long","item_type":"request","status":"needs_action","title":"Choose an option","metadata":{"pause_state_id":"long","input_type":"choice"}}"#)
        let item = ComposerAskItem(id: "long", prompt: "Choose an option", hint: nil, from: nil, taskId: nil,
            form: .choices(choices), source: .attention(base))
        let host = UIHostingController(rootView: ComposerAskDock(items: [item], answer: { _, _ in nil }, onHandoff: { _ in }, onReviewPlan: { _ in }))
        let fitted = host.sizeThatFits(in: CGSize(width: 320, height: 2000))
        XCTAssertLessThan(fitted.height, 400, "Options must scroll instead of pushing the composer beyond the phone")
    }

    @MainActor func testMcpArgumentsAndDecisionsHaveIndependentBoundedViewports() {
        let prompt = "Approve this action?\n\nArguments:\n" + String(repeating: "  argument: value\n", count: 80)
        let choices = ["allow_once", "session:mcp_tool", "always:mcp_tool", "session:capability", "always:capability", "deny"].map {
            ComposerAskChoice(id: $0, label: $0, action: .confirm(true))
        }
        let base = Self.attention(#"{"id":"mcp","item_type":"request","status":"needs_action","title":"Review MCP call","metadata":{"pause_state_id":"mcp","input_type":"choice"}}"#)
        let item = ComposerAskItem(id: "mcp", prompt: prompt, hint: nil, from: nil, taskId: nil,
            form: .choices(choices), source: .attention(base))
        let host = UIHostingController(rootView: ComposerAskDock(items: [item], answer: { _, _ in nil }, onHandoff: { _ in }, onReviewPlan: { _ in }))
        let size = host.sizeThatFits(in: CGSize(width: 320, height: 2000))
        XCTAssertLessThan(size.height, 500, "Arguments must not displace the decision controls")
        XCTAssertLessThanOrEqual(size.width, 320)
    }

    @MainActor func testLongAskChoicesWrapWithinPhoneWidth() {
        let label = String(repeating: "A long option that needs to remain readable ", count: 12)
        let host = UIHostingController(rootView: ComposerAskFlow {
            Button(label) {}.buttonStyle(ChipStyle())
        })
        let size = host.sizeThatFits(in: CGSize(width: 240, height: 1000))
        XCTAssertLessThanOrEqual(size.width, 240)
        XCTAssertGreaterThan(size.height, ChipStyle.minHitSize + 4,
                             "The label must wrap, rather than overflow as a single offscreen chip")
    }
    func testEveryCanonicalAskKindRemainsVisibleInTheComposer() {
        let kinds = ["text", "password", "otp", "choice", "multi_choice", "confirmation", "external_action",
                     "file_path", "guidance", "tool_authorization", "sandbox_override", "diff_approval", "form"]
        let rows = kinds.map { kind in Self.attention("""
        {"id":"\(kind)","item_type":"request","status":"needs_action","title":"Question",
         "metadata":{"input_type":"\(kind)","pause_state_id":"\(kind)",
           "hitl_request":{"scope":{"chat_session_id":"project-chat"}}}}
        """) }
        let items = ComposerAskModel.items(chatEscalations: [], attention: rows, sessionId: "project-chat",
            threadId: nil, chatTaskIds: [], plannerTaskIds: [], readOnly: false)
        XCTAssertEqual(Set(items.map(\.id)), Set(kinds))
        for item in items where ["password", "otp", "tool_authorization", "sandbox_override", "diff_approval", "form"].contains(item.id) {
            guard case .handoff = item.form else { return XCTFail("Secrets and reviews must retain their focused answer screen") }
        }
    }

    func testDismissOnlyNoticesStayInAttentionOutOfTheComposers() {
        func row(_ id: String, _ options: String) -> AttentionItem { Self.attention("""
        {"id":"\(id)","item_type":"request","status":"needs_action","title":"Decision Engine is unavailable",
         "metadata":{"source":"service_health","input_type":"choice","pause_state_id":"\(id)",
           "input_schema":{"type":"choice","options":\(options)},
           "hitl_request":{"scope":{"chat_session_id":"project-chat"}}}}
        """) }
        let notice = row("notice", #"[{"id":"dismiss","label":"Dismiss"}]"#)
        let question = row("question", #"[{"id":"dismiss","label":"Dismiss"},{"id":"retry","label":"Retry"}]"#)
        XCTAssertTrue(notice.isDismissOnly)
        XCTAssertFalse(question.isDismissOnly)
        // Chat (its own session) and VibeDev (no session) composers alike.
        for session in ["project-chat", nil] as [String?] {
            let items = ComposerAskModel.items(chatEscalations: [], attention: [notice, question], sessionId: session,
                threadId: nil, chatTaskIds: [], plannerTaskIds: [], readOnly: false)
            XCTAssertEqual(items.map(\.id), ["question"])
        }
        // Attention's own surfaces still answer it.
        XCTAssertNotNil(ComposerAskModel.item(attention: notice))
    }

    func testVibeDevListsEveryAskWithItsTurnsAndProjectQuestionsFirst() throws {
        let god = try JSONDecoder().decode(PendingUserRequest.self, from: Data(#"{"id":"god-global","request_type":"sandbox_override","question":"Allow edit?","options":[{"id":"allow_once","label":"Allow once"},{"id":"deny","label":"Deny"}],"context":{"input_type":"choice","god_mode":true,"chat_session_id":"other-chat"}}"#.utf8))
        let rows = [AttentionItem(pending: god)] + ["turn-1", "turn-2", "another-run"].map { task in Self.attention("""
        {"id":"ask-\(task)","item_type":"request","status":"needs_action","title":"Continue?","task_id":"\(task)",
         "metadata":{"input_type":"text","pause_state_id":"ask-\(task)"}}
        """) } + [Self.attention("""
        {"id":"project-ask","item_type":"request","status":"needs_action","title":"Which goal?",
         "metadata":{"input_type":"text","pause_state_id":"project-ask","hitl_request":{"scope":{"chat_session_id":"project-chat"}}}}
        """)]
        let items = ComposerAskModel.items(chatEscalations: [], attention: rows, sessionId: "project-chat", threadId: nil,
            chatTaskIds: ["turn-1", "turn-2"], plannerTaskIds: [], readOnly: false)
        XCTAssertEqual(items.map(\.id), ["ask-turn-1", "ask-turn-2", "project-ask", "god-global", "ask-another-run"])
        XCTAssertEqual(items.map(\.elsewhere), [false, false, false, true, true])
        let fresh = ComposerAskModel.items(chatEscalations: [], attention: rows, sessionId: nil, threadId: nil,
            chatTaskIds: [], plannerTaskIds: [], readOnly: false)
        XCTAssertEqual(fresh.map(\.id), ["god-global", "ask-turn-1", "ask-turn-2", "ask-another-run", "project-ask"])
        // A god-mode grant keeps its own "Requested in" line, never the origin line.
        XCTAssertNil(ComposerAskModel.origin(of: try XCTUnwrap(fresh.first), chatTitles: [:]))
        // The display destination does not rewrite the grant's originating scope.
        guard case .attention(let origin) = try XCTUnwrap(fresh.first).source else { return XCTFail("Missing original request") }
        XCTAssertEqual(origin.schema?.context?.chatSessionId, "other-chat")
    }

    func testThreadEditsPreserveUntouchedFieldsAndAllowExplicitMemoryClear() {
        let renamed = ChatThreadSettingsPatch.make(threadID: "general", originalName: "Old", originalMemory: "Saved context", name: "New", memory: "Saved context")
        XCTAssertEqual(renamed["name"], .string("New"))
        XCTAssertNil(renamed.object?["memory_text"])
        let cleared = ChatThreadSettingsPatch.make(threadID: "general", originalName: "Old", originalMemory: "Saved context", name: "Old", memory: "")
        XCTAssertNil(cleared.object?["name"])
        XCTAssertEqual(cleared["memory_text"], .string(""))
        XCTAssertEqual(ChatThreadSettingsPatch.make(threadID: "general", originalName: "Name", originalMemory: "Context", name: " Name ", memory: "Context"), .object([:]))
    }

    func testChatDraftsAreScopedAndClearedAfterSending() {
        let suite = "chat-drafts-" + UUID().uuidString
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = ChatDraftStore(defaults: defaults)
        let first = ChatViewModel.localStorageKey(origin: "https://one", principal: "owner", workspace: "home", session: "one")
        let second = ChatViewModel.localStorageKey(origin: "https://one", principal: "owner", workspace: "home", session: "two")
        store.save("Unsent work", key: first)
        XCTAssertEqual(ChatDraftStore(defaults: defaults).read(first), "Unsent work")
        XCTAssertEqual(store.read(second), "")
        XCTAssertNotEqual(first, ChatViewModel.localStorageKey(origin: "https://two", principal: "owner", workspace: "home", session: "one"))
        XCTAssertNotEqual(first, ChatViewModel.localStorageKey(origin: "https://one", principal: "guest", workspace: "home", session: "one"))
        XCTAssertNotEqual(first, ChatViewModel.localStorageKey(origin: "https://one", principal: "owner", workspace: "work", session: "one"))
        store.save("", key: first)
        XCTAssertEqual(store.read(first), "")
    }

    func testScopedOutputReferencesAndWholeChatAccounting() {
        XCTAssertEqual(ChatSessionAccounting.outputPath("Pages/Chapter-Map.md"), "Pages/Chapter-Map.md")
        XCTAssertEqual(ChatSessionAccounting.outputPath("Chapter%20Map.md"), "Chapter Map.md")
        for raw in ["https://other.test/file.md", "//other.test/file.md", "javascript:evil", "../private.md", "%2e%2e/private.md", "/work?task=x", "/notes?path=x"] {
            XCTAssertNil(ChatSessionAccounting.outputPath(raw), raw)
        }
        XCTAssertNotNil(ChatSessionAccounting.recordedDate("2026-10-23T02:00:00.123456Z"))
        XCTAssertNotNil(ChatSessionAccounting.recordedDate("2026-10-23T02:00:00Z"))
        XCTAssertEqual(ChatSessionAccounting.recordedDate("1000"), Date(timeIntervalSince1970: 1))
        XCTAssertNil(ChatSessionAccounting.recordedDate("unavailable"))
        let sql = ChatSessionAccounting.sql("chat'1")
        XCTAssertTrue(sql.contains("chat_session_id = 'chat''1'"))
        XCTAssertTrue(sql.contains("FROM llm_cost_daily"))
        XCTAssertTrue(sql.contains("unpriced_calls"))
        XCTAssertTrue(sql.contains("CASE WHEN SUM(priced_calls) > 0"))
        let models = ChatSessionAccounting.sql("chat'1", byModel: true)
        XCTAssertTrue(models.contains("chat_session_id = 'chat''1'"))
        XCTAssertTrue(models.contains("GROUP BY provider, model"))
        var draft = WorkQualityDraft()
        draft.maxAttempts = 0; draft.minutes = 0; draft.dollars = 0
        XCTAssertEqual(draft.proposal()["budget"]["max_attempts"], .integer(0))
        XCTAssertEqual(draft.proposal()["budget"]["max_wall_clock_secs"], .integer(0))
        XCTAssertEqual(draft.proposal()["budget"]["max_cost_microusd"], .integer(0))
    }

    func testExecutionModesUseServerStateAndExpireTimedSuperMode() throws {
        let chat = try JSONDecoder().decode(ChatGodModeState.self, from: Data(#"{"enabled":true,"override":null,"default":true}"#.utf8))
        XCTAssertTrue(chat.enabled); XCTAssertNil(chat.overrideChoice); XCTAssertTrue(chat.defaultEnabled)
        let timed = try JSONDecoder().decode(SuperGodModeState.self, from: Data(#"{"active":true,"enabled":true,"until_ms":1000,"remaining_secs":1}"#.utf8))
        XCTAssertTrue(timed.activeAt(Date(timeIntervalSince1970: 0.999)))
        XCTAssertFalse(timed.activeAt(Date(timeIntervalSince1970: 1)))
        XCTAssertEqual(SuperGodModeDuration.hour.rawValue, "1h")
        XCTAssertEqual(SuperGodModeDuration.untilOff.rawValue, "forever")
        XCTAssertThrowsError(try JSONDecoder().decode(SuperGodModeState.self, from: Data(#"{"enabled":true}"#.utf8)))
    }

    func testExecutionModeSaveAlwaysReleasesTheMenu() {
        var gate = ExecutionModeSaveGate()
        let first = gate.begin()
        XCTAssertNotNil(first); XCTAssertTrue(gate.busy)
        XCTAssertNil(gate.begin(), "one save at a time")
        // The chat changed while the save ran: finishing still releases the menu.
        gate.finish(first!)
        XCTAssertFalse(gate.busy)
        // A chat switch resets the gate; the older save finishing later
        // cannot release a save started for the new chat.
        let stale = gate.begin()!
        gate.reset()
        XCTAssertFalse(gate.busy)
        let current = gate.begin()!
        gate.finish(stale)
        XCTAssertTrue(gate.busy)
        gate.finish(current)
        XCTAssertFalse(gate.busy)
    }

    func testChatFileSizesIncludeEmptyFilesAndKeepUnknownSizesExplicit() throws {
        let inventory = try JSONDecoder().decode(WorkJSON.self, from: Data(#"{"files":[{"size":0},{"size":1024},{"size":null}]}"#.utf8))
        XCTAssertEqual(ChatSessionAccounting.fileSize(.object([:])), "Size unavailable")
        XCTAssertNotEqual(ChatSessionAccounting.fileSize(.object(["size": .integer(0)])), "Size unavailable")
        XCTAssertTrue(ChatSessionAccounting.fileSizeSummary(inventory).contains("3 files · Known size:"))
        XCTAssertTrue(ChatSessionAccounting.fileSizeSummary(inventory).contains("1 size unavailable"))
    }

    func testTasklessChatQuestionMatchesItsSessionFromLedgerAndFeed() throws {
        let raw = """
        {"id":"question-1","request_type":"need_user_input","question":"Approve the work plan?",
         "context":{"chat_session_id":"chat-1","input_type":"confirmation",
                    "input_schema":{"confirm_label":"Approve","deny_label":"Deny"}}}
        """
        let pending = try JSONDecoder().decode(PendingUserRequest.self, from: Data(raw.utf8))
        let row = AttentionItem(pending: pending)
        XCTAssertTrue(ComposerAskModel.belongs(row, sessionId: "chat-1", threadId: "general", chatTaskIds: []))
        XCTAssertFalse(ComposerAskModel.belongs(row, sessionId: "chat-2", threadId: "general", chatTaskIds: []))
        XCTAssertFalse(ComposerAskModel.belongs(row, sessionId: nil, threadId: "general", chatTaskIds: []))
        let roundTrip = try JSONDecoder().decode(AttentionItem.self, from: JSONEncoder().encode(row))
        XCTAssertTrue(ComposerAskModel.belongs(roundTrip, sessionId: "chat-1", threadId: nil, chatTaskIds: []))
        let projected = Self.attention("""
        {"id":"feed-question","item_type":"request","status":"needs_action","title":"Approve the work plan?",
         "metadata":{"source":"user_request","input_type":"confirmation","pause_state_id":"question-1",
                     "input_schema":{"context":{"chat_session_id":"chat-1"}}}}
        """)
        XCTAssertTrue(ComposerAskModel.belongs(projected, sessionId: "chat-1", threadId: nil, chatTaskIds: []))
        XCTAssertFalse(ComposerAskModel.belongs(projected, sessionId: "chat-2", threadId: nil, chatTaskIds: []))
    }

    func testGodScopePickerPreservesExactIdsAndFullLongPaths() {
        let path = "/notes/" + String(repeating: "long-folder/", count: 40) + "plan.md"
        let options = [AttentionGrantOption(id: "allow_once", label: "Allow once"),
                       AttentionGrantOption(id: "session:file", label: "This session: \(path)"),
                       AttentionGrantOption(id: "always:file", label: "Always: \(path)"),
                       AttentionGrantOption(id: "session:folder", label: "This session: anything under /notes"),
                       AttentionGrantOption(id: "always:folder", label: "Always: anything under /notes"),
                       AttentionGrantOption(id: "deny", label: "Deny")]
        let scopes = GodGrantScope.grouped(options)
        XCTAssertEqual(scopes.map(\.id), ["file", "folder"])
        XCTAssertEqual(scopes.first?.detail, path)
        XCTAssertEqual(scopes.first?.sessionId, "session:file")
        XCTAssertEqual(scopes.last?.alwaysId, "always:folder")
        XCTAssertTrue(GodGrantScope.grouped(options.filter { $0.id != "always:file" }).isEmpty)
        XCTAssertTrue(GodGrantScope.grouped(options + [AttentionGrantOption(id: "new_option", label: "New choice")]).isEmpty)
    }

    func testExactShellScopeKeepsBothApprovalDurations() {
        let scopes = GodGrantScope.grouped([
            AttentionGrantOption(id: "allow_once", label: "Allow once"),
            AttentionGrantOption(id: "session:exact", label: "This session: this exact shell invocation"),
            AttentionGrantOption(id: "always:exact", label: "Always: this exact shell invocation"),
            AttentionGrantOption(id: "deny", label: "Deny")
        ])
        XCTAssertEqual(scopes.count, 1)
        XCTAssertEqual(scopes.first?.label, "Exact command")
        XCTAssertEqual(scopes.first?.sessionId, "session:exact")
        XCTAssertEqual(scopes.first?.alwaysId, "always:exact")
    }

    func testGodFileApprovalKeepsEveryChoiceAndOriginInEveryChat() throws {
        let raw = """
        {"id":"god-file","request_type":"sandbox_override","question":"Apply edits?",
         "options":[{"id":"allow_once","label":"Allow once"},{"id":"session:file","label":"This session: plan.md"},
                    {"id":"always:file","label":"Always: plan.md"},{"id":"deny","label":"Deny"}],
         "context":{"input_type":"choice","god_mode":true,"chat_session_id":"chat-1","command":"/notes/programs/plan.md",
                    "files":[{"path":"/notes/programs/plan.md","unified_diff":"+new plan"}]}}
        """
        let pending = try JSONDecoder().decode(PendingUserRequest.self, from: Data(raw.utf8))
        var row = AttentionItem(pending: pending)
        row.uiThreadId = "general"
        row.taskId = "task-1"
        XCTAssertTrue(row.isGodMode)
        XCTAssertEqual(row.inputType, "choice")
        XCTAssertEqual(row.renderKind, "sandbox_override")
        XCTAssertEqual(row.grantSubject, "/notes/programs/plan.md")
        let item = try XCTUnwrap(ComposerAskModel.item(attention: row))
        guard case .choices(let choices) = item.form else { return XCTFail("God grants must be answered in the composer") }
        XCTAssertEqual(choices.map(\.id), ["allow_once", "session:file", "always:file", "deny"])
        XCTAssertEqual(item.files.first?.unifiedDiff, "+new plan")
        XCTAssertTrue(ComposerAskModel.belongs(row, sessionId: "chat-1", threadId: "general", chatTaskIds: []))
        XCTAssertTrue(ComposerAskModel.belongs(row, sessionId: "chat-2", threadId: "other", chatTaskIds: []))
        XCTAssertEqual(row.schema?.context?.chatSessionId, "chat-1")
    }

    // MARK: Fixtures

    static func content(_ json: String) -> ChatMessageContentData {
        try! JSONDecoder().decode(ChatMessageContentData.self, from: Data(json.utf8))
    }

    static func attention(_ json: String) -> AttentionItem {
        try! JSONDecoder().decode(AttentionItem.self, from: Data(json.utf8))
    }

    static let confirmCard = content("""
    {"type":"escalation","text":"Action required","question":"Deploy to staging now?",
     "execution_id":"exec-1","pause_state_id":"pause-1","escalation_type":"approval",
     "input_type":"confirmation",
     "options":[{"id":"yes","label":"Deploy","action":{"type":"respond_confirmation","confirmed":true}},
                {"id":"no","label":"Not yet","action":{"type":"respond_confirmation","confirmed":false}}]}
    """)
    static let textCard = content("""
    {"type":"escalation","question":"Which branch should I use?","execution_id":"exec-2",
     "pause_state_id":"pause-2","escalation_type":"approval","input_type":"text"}
    """)
    static let passwordCard = content("""
    {"type":"escalation","question":"Enter the staging password","execution_id":"exec-3",
     "pause_state_id":"pause-3","escalation_type":"approval","input_type":"password"}
    """)
    static let resolvedCard = content("""
    {"type":"escalation","question":"Old question","execution_id":"exec-4","pause_state_id":"pause-4",
     "escalation_type":"approval","input_type":"text","resolved":true}
    """)
    static let planApproval = attention("""
    {"id":"att-plan","title":"Plan ready","item_type":"request","status":"needs_action",
     "metadata":{"source":"plan_approval","input_type":"confirmation",
                 "question":"Approve the plan for Ship v2?","pause_state_id":"plan-1","task_id":"task-9"}}
    """)
    static let sessionScopedAsk = attention("""
    {"id":"att-scope","title":"Pick one","item_type":"request","status":"needs_action",
     "metadata":{"source":"agentic","input_type":"choice","question":"Which region?",
                 "pause_state_id":"pause-scope",
                 "options":[{"id":"eu","label":"EU"},{"id":"us","label":"US"}],
                 "hitl_request":{"scope":{"session_id":"session-1"},"delegated_agent_id":"researcher"},
                 "delegation_child":true,"delegated_agent_id":"researcher"}}
    """)
    static let unrelatedAsk = attention("""
    {"id":"att-other","title":"Elsewhere","item_type":"request","status":"needs_action",
     "metadata":{"source":"agentic","input_type":"text","question":"Not this chat",
                 "pause_state_id":"pause-other","task_id":"task-elsewhere"}}
    """)
    static let duplicateOfChat = attention("""
    {"id":"att-dup","title":"Dup","item_type":"request","status":"needs_action",
     "metadata":{"source":"agentic","input_type":"text","question":"Which branch should I use?",
                 "pause_state_id":"pause-2","task_id":"task-1"}}
    """)

    private func items(
        chat: [ChatMessageContentData] = [confirmCard, textCard],
        attention: [AttentionItem] = [],
        chatTaskIds: Set<String> = ["task-1", "task-9"],
        plannerTaskIds: Set<String> = [],
        threadId: String = "general",
        readOnly: Bool = false
    ) -> [ComposerAskItem] {
        ComposerAskModel.items(
            chatEscalations: chat, attention: attention, sessionId: "session-1", threadId: threadId,
            chatTaskIds: chatTaskIds, plannerTaskIds: plannerTaskIds, readOnly: readOnly)
    }

    // MARK: Queue title

    func testQueueTitleFoldsTheFirstMessageOntoOneLine() {
        XCTAssertEqual(QueuedMessagesTitle.make(count: 2, firstText: "Fix the\n build   now "),
                       "2 queued · Fix the build now")
        XCTAssertEqual(QueuedMessagesTitle.make(count: 1, firstText: nil), "1 queued · (attachments only)")
        XCTAssertEqual(QueuedMessagesTitle.make(count: 0, firstText: "x"), "")
        let long = String(repeating: "a", count: 300)
        let preview = QueuedMessagesTitle.preview(long)
        XCTAssertEqual(preview.count, QueuedMessagesTitle.previewLimit)
        XCTAssertTrue(preview.hasSuffix("…"))
    }

    // MARK: Countdown

    func testCountdownDrainsFromFullToEmpty() {
        let now = Date()
        XCTAssertEqual(AutoSendCountdown.fraction(deadline: now.addingTimeInterval(3), total: 3, now: now), 1)
        XCTAssertEqual(AutoSendCountdown.fraction(deadline: now.addingTimeInterval(1.5), total: 3, now: now), 0.5, accuracy: 0.0001)
        XCTAssertEqual(AutoSendCountdown.fraction(deadline: now.addingTimeInterval(-1), total: 3, now: now), 0)
        XCTAssertEqual(AutoSendCountdown.fraction(deadline: now.addingTimeInterval(9), total: 3, now: now), 1)
        XCTAssertEqual(AutoSendCountdown.fraction(deadline: now, total: 0, now: now), 0)
        XCTAssertEqual(AutoSendCountdown.steppedFraction(remaining: 3), 1)
        XCTAssertEqual(AutoSendCountdown.steppedFraction(remaining: 1), 1.0 / 3.0, accuracy: 0.0001)
        XCTAssertEqual(AutoSendCountdown.steppedFraction(remaining: -2), 0)
        XCTAssertEqual(AutoSendCountdown.label(remaining: 2), "Sending in 2s…")
    }

    // MARK: Presence

    func testPresencePrecedenceMatchesWeb() {
        XCTAssertEqual(ChatPresenceState.derive(hasPendingAsk: true, streaming: true, toolsRunning: true, liveTurn: true), .needsYou)
        XCTAssertEqual(ChatPresenceState.derive(hasPendingAsk: false, streaming: true, toolsRunning: false, liveTurn: true), .working)
        XCTAssertEqual(ChatPresenceState.derive(hasPendingAsk: false, streaming: false, toolsRunning: true, liveTurn: false), .working)
        XCTAssertEqual(ChatPresenceState.derive(hasPendingAsk: false, streaming: false, toolsRunning: false, liveTurn: true), .thinking)
        XCTAssertEqual(ChatPresenceState.derive(hasPendingAsk: false, streaming: false, toolsRunning: false, liveTurn: false), .idle)
        XCTAssertEqual(ChatPresenceState.needsYou.label(assistant: "Magican"), "Magican needs you")
    }

    // MARK: Ask dock items

    func testChatCardsBecomeInlineAsksAndSecretsHandOff() {
        let list = items(chat: [Self.confirmCard, Self.textCard, Self.passwordCard, Self.resolvedCard])
        XCTAssertEqual(list.map(\.id), ["pause-1", "pause-2", "pause-3"], "a resolved card asks nothing")
        guard case .choices(let choices) = list[0].form else { return XCTFail("confirmation → choices") }
        XCTAssertEqual(choices.map(\.label), ["Deploy", "Not yet"])
        XCTAssertTrue(choices[0].prominent)
        XCTAssertTrue(choices[1].destructive)
        guard case .text(_, let guidance) = list[1].form else { return XCTFail("text → field") }
        XCTAssertFalse(guidance)
        XCTAssertEqual(list[2].form, .handoff("Enter it securely →"))
    }

    func testEveryFeedAskIsListedThisConversationsFirst() {
        let list = items(chat: [], attention: [Self.unrelatedAsk, Self.planApproval, Self.sessionScopedAsk])
        XCTAssertEqual(list.map(\.id), ["plan-1", "pause-scope", "pause-other"])
        XCTAssertEqual(list.map(\.elsewhere), [false, false, true])
        guard case .choices(let plan) = list[0].form else { return XCTFail("plan approval → choices") }
        XCTAssertEqual(plan.map(\.label), ["Review plan", "Reject", "Approve", "Approve & Run"])
        XCTAssertEqual(list[0].taskId, "task-9")
        XCTAssertEqual(list[1].from, "from researcher")
    }

    /// Two untitled chats share the `general` thread: a sibling chat's
    /// question stays there, labelled, and never takes this chat's typed text.
    func testSiblingChatQuestionInTheSharedThreadIsElsewhere() {
        let sibling = Self.attention("""
        {"id":"att-sibling-chat","title":"Q","item_type":"request","status":"needs_action",
         "ui_thread_id":"general","chat_session_id":"session-2",
         "metadata":{"input_type":"text","question":"Which city?","pause_state_id":"pause-sibling-chat",
                     "hitl_request":{"scope":{"thread_id":"general","ui_thread_id":"general"}}}}
        """)
        let own = Self.attention("""
        {"id":"att-own-chat","title":"Q","item_type":"request","status":"needs_action",
         "ui_thread_id":"general","chat_session_id":"session-1",
         "metadata":{"input_type":"text","question":"Which date?","pause_state_id":"pause-own-chat"}}
        """)
        let unstamped = Self.attention("""
        {"id":"att-thread-only","title":"Q","item_type":"request","status":"needs_action",
         "ui_thread_id":"general",
         "metadata":{"input_type":"text","question":"Anyone?","pause_state_id":"pause-thread-only"}}
        """)
        let list = items(chat: [], attention: [sibling, own, unstamped], chatTaskIds: [])
        XCTAssertEqual(list.map(\.id), ["pause-own-chat", "pause-sibling-chat", "pause-thread-only"])
        XCTAssertEqual(list.map(\.elsewhere), [false, true, true])
        XCTAssertEqual(ComposerAskModel.origin(of: list[1], chatTitles: ["session-2": "Trip planning"]),
                       "From Trip planning")
        XCTAssertNil(ComposerAskModel.origin(of: list[0], chatTitles: [:]))
        // Another chat's question is never here, even in a shared named
        // thread or under a task this chat knows.
        var tasked = sibling
        tasked.uiThreadId = "launch"
        tasked.taskId = "task-1"
        XCTAssertFalse(ComposerAskModel.belongsHere(tasked, sessionId: "session-1", threadId: "launch",
                                                    chatTaskIds: ["task-1"]))
    }

    func testPlanApprovalTheplannerDockOffersIsLeftToIt() {
        let list = items(chat: [], attention: [Self.planApproval], plannerTaskIds: ["task-9"])
        XCTAssertTrue(list.isEmpty)
        // Another conversation's plan approval is still offered, from elsewhere.
        let foreign = items(chat: [], attention: [Self.planApproval], chatTaskIds: [], plannerTaskIds: ["task-1"])
        XCTAssertEqual(foreign.map(\.id), ["plan-1"])
        XCTAssertTrue(foreign[0].elsewhere)
    }

    func testAskFromElsewhereSaysWhereItCameFrom() {
        let titles = ["chat-other": "Trip planning"]
        func origin(_ json: String) -> String? {
            items(chat: [], attention: [Self.attention(json)]).first
                .flatMap { ComposerAskModel.origin(of: $0, chatTitles: titles) }
        }
        XCTAssertEqual(origin("""
        {"id":"a","title":"Q","item_type":"request","status":"needs_action","chat_session_id":"chat-other",
         "metadata":{"input_type":"text","pause_state_id":"a","hitl_request":{"scope":{"agent_id":"researcher"}}}}
        """), "From Trip planning · researcher")
        XCTAssertEqual(origin("""
        {"id":"b","title":"Q","item_type":"request","status":"needs_action",
         "metadata":{"input_type":"text","pause_state_id":"b","hitl_request":{"scope":{"chat_session_id":"ab12cd34ef56"}}}}
        """), "From Chat ab12cd34")
        XCTAssertEqual(origin("""
        {"id":"c","title":"Q","item_type":"request","status":"needs_action","task_id":"task-elsewhere-123",
         "metadata":{"input_type":"text","pause_state_id":"c"}}
        """), "From task task-elsewher")
        XCTAssertEqual(origin("""
        {"id":"d","title":"Q","item_type":"request","status":"needs_action",
         "metadata":{"input_type":"text","pause_state_id":"d","input_schema":{"context":{"session":"run:0123456789"}}}}
        """), "From Run 01234567 · background run")
        // This conversation's own ask shows no origin line, nor does a chat
        // card in this transcript whose feed row carries no stamp of it.
        XCTAssertNil(origin("""
        {"id":"f","title":"Q","item_type":"request","status":"needs_action","task_id":"task-1",
         "metadata":{"input_type":"text","pause_state_id":"f"}}
        """))
        let card = items(chat: [Self.confirmCard], attention: [Self.attention("""
        {"id":"e","title":"Deploy?","item_type":"request","status":"needs_action",
         "metadata":{"input_type":"confirmation","pause_state_id":"pause-1"}}
        """)], chatTaskIds: [])
        XCTAssertEqual(card.map(\.id), ["pause-1"])
        XCTAssertFalse(card[0].elsewhere)
    }

    func testChatCardAndFeedRowForOneAskShowOnce() {
        let list = items(chat: [Self.textCard], attention: [Self.duplicateOfChat])
        XCTAssertEqual(list.map(\.id), ["pause-2"])
        guard case .attention = list[0].source else { return XCTFail("the canonical contract must win") }
    }

    func testCanonicalApprovalPreviewCannotBeHiddenByAChatConfirmation() {
        let row = Self.attention("""
        {"id":"richer","title":"Review change","item_type":"request","status":"needs_action",
         "task_id":"task-1","metadata":{"source":"agentic","input_type":"confirmation",
         "pause_state_id":"pause-1","input_schema":{"approval_preview":{"title":"Revise program","evidence":[],"warnings":[],"notes":[]}}}}
        """)
        let list = items(chat: [Self.confirmCard], attention: [row])
        XCTAssertEqual(list.count, 1)
        guard case .attention = list[0].source else { return XCTFail("canonical preview must win") }
        guard case .handoff = list[0].form else { return XCTFail("approval must show the preview before consent") }
    }

    func testCanonicalTargetIdAndAliasesAreRetainedWithoutARespondHandle() {
        let row = Self.attention("""
        {"id":"feed","title":"Choose","item_type":"request","status":"needs_action","task_id":"task-1",
         "metadata":{"source":"agentic","hitl_request":{"id":"canonical","source":"agentic","input_type":"text",
         "identifiers":{"pause_state_id":"pause-2"},"prompt":"New wording"}}}
        """)
        XCTAssertEqual(row.correlationId, "canonical")
        XCTAssertTrue(row.askIdentifiers.contains("pause-2"))
        let list = items(chat: [Self.textCard], attention: [row])
        XCTAssertEqual(list.count, 1)
        XCTAssertEqual(list[0].prompt, "New wording")
    }

    func testArchivedConversationAsksNothing() {
        XCTAssertTrue(items(attention: [Self.planApproval], readOnly: true).isEmpty)
    }

    func testFailedFeedRowsCannotBecomeComposerQuestions() throws {
        for kind in ["task", "escalation", "request"] {
            let row = try JSONDecoder().decode(AttentionItem.self, from: Data("""
            {"id":"failed-fixture","title":"Waiting on you","item_type":"\(kind)","status":"failed",
             "metadata":{"source":"agentic","input_type":"confirmation","pause_state_id":"old-pause"}}
            """.utf8))
            XCTAssertFalse(row.isActionable)
            XCTAssertNil(ComposerAskModel.item(attention: row))
        }
    }

    func testSwitcherKeepsTheActiveQuestionAndWraps() {
        let list = items(chat: [Self.confirmCard, Self.textCard, Self.passwordCard])
        XCTAssertEqual(ComposerAskModel.activeId(current: "pause-2", in: list), "pause-2")
        XCTAssertEqual(ComposerAskModel.activeId(current: "gone", in: list), "pause-1")
        XCTAssertNil(ComposerAskModel.activeId(current: nil, in: []))
        XCTAssertEqual(ComposerAskModel.step(from: "pause-1", by: -1, in: list), "pause-3")
        XCTAssertEqual(ComposerAskModel.step(from: "pause-3", by: 1, in: list), "pause-1")
        XCTAssertEqual(ComposerAskModel.position(of: "pause-2", in: list), "2 of 3")
        XCTAssertNil(ComposerAskModel.position(of: "pause-1", in: [list[0]]))
    }

    func testChatRepliesComposeTheCanonicalSubmission() {
        let list = items(chat: [Self.confirmCard, Self.textCard])
        guard case .choices(let choices) = list[0].form else { return XCTFail() }
        XCTAssertEqual(ComposerAskModel.submission(for: Self.confirmCard, reply: .choice(choices[0])),
                       .response(.confirmation(true)))
        XCTAssertEqual(ComposerAskModel.submission(for: Self.textCard, reply: .text("  main ")),
                       .response(.text("main")))
        XCTAssertNil(ComposerAskModel.submission(for: Self.textCard, reply: .text("   ")))
    }

    // MARK: Ask dock: thread scoping, review links, approve / reject

    /// A Relentless review ask as the feed carries it: only `task_id` and the
    /// row's own `ui_thread_id`, no session / thread scope on the request.
    static let relentlessReview = attention("""
    {"id":"att-review","title":"Review needed","item_type":"request","status":"needs_action",
     "task_id":"task-relentless","ui_thread_id":"launch",
     "metadata":{"source":"user_request",
       "hitl_request":{"correlation_id":"exec-9:relentless-review:run-7:hash","input_type":"choice",
         "prompt":"An accepted draft needs your exact review.",
         "input_schema":{"request_type":"relentless.accepted_review",
           "options":[{"id":"approve","label":"Approve draft"},{"id":"reject","label":"Reject draft"}],
           "context":{"run_id":"run-7","artifact_hash":"hash","task_id":"task-relentless",
                      "review_href":"/work?view=quality&run=run-7&task=task-relentless",
                      "review_label":"Review draft","nested":{"odd":[1,2]}}}}}}
    """)
    /// A sibling ask of the same task that carries only `task_id`.
    static let siblingOfRelentless = attention("""
    {"id":"att-sibling","title":"Sibling","item_type":"request","status":"needs_action",
     "task_id":"task-relentless",
     "metadata":{"source":"agentic","input_type":"text","question":"Anything else?",
                 "pause_state_id":"pause-sibling"}}
    """)
    static let otherThreadAsk = attention("""
    {"id":"att-thread","title":"Other thread","item_type":"request","status":"needs_action",
     "task_id":"task-other","ui_thread_id":"research",
     "metadata":{"source":"agentic","input_type":"text","question":"Not here",
                 "pause_state_id":"pause-thread"}}
    """)

    func testFeedAskBelongsByItsOwnUiThreadIdAndItsTasksSiblingsFollow() {
        let list = items(chat: [], attention: [Self.relentlessReview, Self.siblingOfRelentless,
                                               Self.otherThreadAsk, Self.unrelatedAsk],
                         chatTaskIds: [], threadId: "launch")
        XCTAssertEqual(list.filter { !$0.elsewhere }.map(\.id), ["exec-9:relentless-review:run-7:hash", "pause-sibling"],
                       "the row's ui_thread_id places it here; its task's other asks follow")
        XCTAssertEqual(list.filter(\.elsewhere).map(\.id), ["pause-thread", "pause-other"])
        XCTAssertTrue(ComposerAskModel.belongs(Self.relentlessReview, sessionId: nil, threadId: "launch",
                                               chatTaskIds: []))
        XCTAssertFalse(ComposerAskModel.belongs(Self.otherThreadAsk, sessionId: nil, threadId: "launch",
                                                chatTaskIds: []))
        XCTAssertTrue(ComposerAskModel.belongs(Self.siblingOfRelentless, sessionId: nil, threadId: "launch",
                                               chatTaskIds: ["task-relentless"]))
        // The shared default thread alone never places an ask here.
        var shared = Self.relentlessReview
        shared.uiThreadId = "general"
        XCTAssertFalse(ComposerAskModel.belongs(shared, sessionId: nil, threadId: "general", chatTaskIds: []))
    }

    func testRelentlessReviewAskOffersApproveRejectAndReviewDraft() throws {
        let item = try XCTUnwrap(ComposerAskModel.item(attention: Self.relentlessReview))
        guard case .choices(let choices) = item.form else { return XCTFail("choice → inline chips") }
        XCTAssertEqual(choices.map(\.label), ["Approve draft", "Reject draft"])
        XCTAssertTrue(choices[0].prominent)
        XCTAssertTrue(choices[1].destructive)
        guard case .option(let option) = choices[0].action else { return XCTFail() }
        XCTAssertEqual(option.id, "approve")

        let review = try XCTUnwrap(item.review)
        XCTAssertEqual(review.label, "Review draft")
        let target = try XCTUnwrap(review.workTarget, "a /work link opens in-app")
        XCTAssertEqual(target.kind, .qualityRun)
        XCTAssertEqual(target.recordID, "run-7")
        XCTAssertEqual(target.section, .quality)
        XCTAssertNil(review.externalURL)
        XCTAssertEqual(Self.relentlessReview.reviewLink, review, "Attention's sheet offers the same link")
    }

    func testAChatCardCarriesItsReviewLinkAndOddContextNeverDropsIt() throws {
        let card = Self.content("""
        {"type":"escalation","question":"Approve the draft?","execution_id":"exec-9",
         "pause_state_id":"pause-review","escalation_type":"approval","input_type":"choice",
         "options":[{"id":"approve","label":"Approve draft"},{"id":"reject","label":"Reject draft"}],
         "input_schema":{"type":"choice","context":{"review_href":"/work?view=quality&run=run-7",
                                                    "review_label":"Review draft","n":3}}}
        """)
        let item = try XCTUnwrap(ComposerAskModel.item(chat: card))
        XCTAssertEqual(item.review?.label, "Review draft")
        XCTAssertEqual(item.review?.workTarget?.kind, .qualityRun)

        let odd = Self.content("""
        {"type":"escalation","question":"Q?","execution_id":"exec-odd","pause_state_id":"pause-odd",
         "escalation_type":"approval","input_type":"text",
         "input_schema":{"type":"text","placeholder":"say","context":"not-an-object"}}
        """)
        XCTAssertEqual(odd.inputSchema?.placeholder, "say", "a non-object context keeps the schema")
        let oddItem = try XCTUnwrap(ComposerAskModel.item(chat: odd))
        XCTAssertNil(oddItem.review)
    }

    func testReviewLinksOpenOnlyWorkDestinationsOrWebPages() {
        XCTAssertNil(HitlReviewLink(href: "/somewhere/else", label: "Look"), "nothing to open")
        XCTAssertNil(HitlReviewLink(href: "  ", label: "Look"))
        let web = HitlReviewLink(href: "https://example.com/run", label: nil)
        XCTAssertEqual(web?.externalURL?.host, "example.com")
        XCTAssertEqual(web?.label, "Review")
    }

    // MARK: Planner dock

    static func entry(_ id: String, _ status: PlanStatus?, questions: Int = 0) -> PlannerDockEntry {
        PlannerDockEntry(
            taskId: id, title: "Task \(id)", planId: "plan-\(id)", status: status,
            questions: (0..<questions).map { PlanQuestion(id: "\(id)-q\($0)", text: "Question \($0)?", options: [], sourceSlotId: nil, urgency: nil) })
    }

    func testPlannerDockShowsQuestionsThenDrafts() {
        let entries = [Self.entry("a", .draft), Self.entry("b", .approved), Self.entry("c", .eliciting, questions: 2),
                       Self.entry("d", .eliciting)]
        let dock = PlannerDockModel.dockEntries(entries)
        XCTAssertEqual(dock.map(\.taskId), ["c", "a"])
        XCTAssertEqual(PlannerDockModel.summary(dock), "3 items across 2 tasks")
        XCTAssertEqual(PlannerDockModel.summary([Self.entry("a", .draft)]), "1 item")
    }

    func testPlanReplyGoesStaleWhenItsQuestionLeaves() {
        let reply = PlanReplyTarget(taskId: "c", taskTitle: "Task c", questionId: "c-q1", questionText: "Q?")
        XCTAssertFalse(PlannerDockModel.isStale(reply, entries: [Self.entry("c", .eliciting, questions: 2)]))
        XCTAssertTrue(PlannerDockModel.isStale(reply, entries: [Self.entry("c", .draft)]))
        XCTAssertFalse(PlannerDockModel.isStale(reply, entries: []), "unknown is not stale")
    }

    func testPlannerReadsOnlyOpenTasksNewestFirst() {
        let tasks = [
            TaskStatusModel(taskId: "old", title: "Old", status: "planning", steps: []),
            TaskStatusModel(taskId: "done", title: "Done", status: "completed", steps: []),
            TaskStatusModel(taskId: "run", title: "Run", status: "running", steps: []),
            TaskStatusModel(taskId: "old", title: "Old", status: "planning", steps: []),
            TaskStatusModel(taskId: "new", title: "New", status: "ready", steps: []),
        ]
        XCTAssertEqual(PlannerDockModel.candidateTaskIds(tasks).map(\.taskId), ["new", "old"])
    }
}

// MARK: - Renders

/// Renders the composer stack's states offscreen for a visual check, in
/// Sorbet light and dark. PNGs land in `COMPOSER_SHOTS_DIR` (or the temp dir).
@MainActor
final class ComposerStackRenderTests: XCTestCase {
    private var savedTheme = ""

    override func setUp() async throws {
        savedTheme = ThemeManager.shared.currentThemeName
    }

    override func tearDown() async throws {
        ThemeManager.shared.applyTheme(savedTheme)
    }

    private var outputDir: URL {
        let env = ProcessInfo.processInfo.environment["COMPOSER_SHOTS_DIR"]
            ?? "/private/tmp/claude-502/-Users-owner-Development-magicbeanbs100x-magician/e62b6512-d9f0-45ce-87a2-9802d3bcdc01/scratchpad/ios-shots"
        let url = URL(fileURLWithPath: env)
        if (try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)) != nil { return url }
        return FileManager.default.temporaryDirectory
    }

    /// `height` nil fits the content; a fixed height gives flexible pieces
    /// (the mention picker's scroll view) the room the chat gives them.
    private func render<V: View>(_ view: V, name: String, width: CGFloat = 393, height: CGFloat? = nil) throws {
        let root = view.frame(width: width, height: height)
        let host = UIHostingController(rootView: root)
        let fitted = host.sizeThatFits(in: CGSize(width: width, height: 3000))
        let size = CGSize(width: width, height: height ?? max(80, fitted.height))
        let window: UIWindow
        if let scene = UIApplication.shared.connectedScenes.compactMap({ $0 as? UIWindowScene }).first {
            window = UIWindow(windowScene: scene)
        } else {
            window = UIWindow()
        }
        window.frame = CGRect(origin: .zero, size: size)
        window.overrideUserInterfaceStyle = ThemeManager.shared.isDark ? .dark : .light
        window.rootViewController = host
        window.isHidden = false
        host.view.frame = window.bounds
        host.view.layoutIfNeeded()
        RunLoop.main.run(until: Date().addingTimeInterval(0.6))
        host.view.layoutIfNeeded()
        let format = UIGraphicsImageRendererFormat()
        format.scale = 3
        let image = UIGraphicsImageRenderer(bounds: window.bounds, format: format).image { _ in
            _ = window.drawHierarchy(in: window.bounds, afterScreenUpdates: true)
        }
        window.isHidden = true
        let url = outputDir.appendingPathComponent("\(name).png")
        try XCTUnwrap(image.pngData()).write(to: url)
        print("composer render: \(url.path)")
    }

    private func fixture(mention: Bool, plannerExpanded: Bool, reply: PlanReplyTarget?) -> some View {
        let planner = PlannerDockLoader()
        planner.setEntries([
            ComposerStackTests.entry("t1", .eliciting, questions: 2),
            ComposerStackTests.entry("t2", .draft),
        ])
        let concurrent = ConcurrentVoiceCoordinator()
        concurrent.error = "Voice requests are unavailable right now."
        let ask = ComposerAskModel.items(
            chatEscalations: [ComposerStackTests.confirmCard, ComposerStackTests.textCard],
            attention: [], sessionId: "session-1", threadId: "general",
            chatTaskIds: [], plannerTaskIds: [], readOnly: false)
        return ComposerStackFixture(
            askItems: ask, planner: planner, concurrent: concurrent,
            mention: mention, plannerExpanded: plannerExpanded, reply: reply)
    }

    func testRenderComposerStackSorbetLightAndDark() throws {
        for theme in ["sorbet", "sorbet-dark"] {
            ThemeManager.shared.applyTheme(theme)
            RunLoop.main.run(until: Date().addingTimeInterval(0.2))
            try render(fixture(mention: true, plannerExpanded: false, reply: nil), name: "composer-stack-\(theme)",
                       height: 1180)
            try render(fixture(mention: false, plannerExpanded: true,
                               reply: PlanReplyTarget(taskId: "t1", taskTitle: "Task t1", questionId: "t1-q0",
                                                      questionText: "Question 0?")),
                       name: "composer-planner-\(theme)", height: 1000)
            try render(ArchivedFixture(), name: "composer-archived-\(theme)", height: 140)
        }
    }
}

/// The composer stack, top to bottom as the chat lays it out: ask dock,
/// planner dock, mention picker, then the card with its top rows (queue →
/// concurrent voice → origin link → banners → plan band) over attachments,
/// input and tools.
private struct ComposerStackFixture: View {
    let askItems: [ComposerAskItem]
    let planner: PlannerDockLoader
    let concurrent: ConcurrentVoiceCoordinator
    let mention: Bool
    let plannerExpanded: Bool
    let reply: PlanReplyTarget?
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        VStack(spacing: 0) {
            Spacer(minLength: 0)
            Text("Here is the release checklist. I need two answers before I deploy.")
                .font(.themed(15))
                .foregroundColor(theme.textColor)
                .padding(12)
                .background(RoundedRectangle(cornerRadius: Lively.bubbleRadius).fill(theme.cardColor))
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(16)
            ComposerAskDock(items: askItems, answer: { _, _ in nil }, onHandoff: { _ in }, onReviewPlan: { _ in })
            ComposerPlannerDock(loader: planner, activeReply: reply, onReply: { _, _ in }, onOpenPlan: { _ in },
                                startsExpanded: plannerExpanded)
            ComposerView(
                text: .constant(mention ? "Ask @re" : "Ship it after the tests"),
                mode: .constant(.plan),
                doPermission: .constant(.ask),
                isThinking: false,
                profiles: [],
                selectedProfile: .constant(""),
                chatHarnesses: [],
                selectedHarnessEngine: .constant("magician"),
                selectedHarnessModel: .constant("default"),
                onSend: {}, onStop: {}, onAttach: {}, onCamera: {},
                mentionItems: [
                    ComposerMentionItem(id: "a1", label: "researcher", detail: "Deep web research", kind: .agent, insertText: "@researcher"),
                    ComposerMentionItem(id: "a2", label: "release-manager", detail: "Ships builds", kind: .agent, insertText: "@release-manager"),
                ],
                stagedAttachments: [
                    StagedAttachment(id: UUID(), remoteId: "r1", filename: "notes.pdf", mime: "application/pdf", thumbnail: nil, uploading: false),
                    StagedAttachment(id: UUID(), remoteId: nil, filename: "log.txt", mime: "text/plain", thumbnail: nil, uploading: true),
                    StagedAttachment(id: UUID(), remoteId: nil, filename: "bad.csv", mime: "text/csv", thumbnail: nil, uploading: false, failed: true),
                ],
                isVoiceMode: .constant(false),
                voiceQueue: AnyView(topRows)
            )
        }
        .background(theme.washGradient)
    }

    private var topRows: some View {
        VStack(spacing: 0) {
            ComposerTopRow(title: QueuedMessagesTitle.make(count: 2, firstText: "Also update the changelog\nand bump the version"),
                           systemImage: "chevron.right") {}
            ConcurrentVoiceStrip(coordinator: concurrent, onReview: { _ in }, topCornerRadius: 0)
            ComposerTopRow(title: "Concurrent · Started from original conversation", systemImage: "arrow.up.right",
                           topCornerRadius: 0) {}
            if let reply {
                PlanReplyBanner(target: reply, onCancel: {})
            }
            AutoSendCountdownBand(remaining: 2, deadline: nil, onCancel: {})
            if reply == nil { PlanModeBand() }
        }
    }
}

private struct ArchivedFixture: View {
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        VStack(spacing: 0) {
            Spacer().frame(height: 24)
            ArchivedSessionBand(onReturn: {})
        }
        .background(theme.washGradient)
    }
}
