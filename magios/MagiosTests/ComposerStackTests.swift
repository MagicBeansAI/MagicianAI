import SwiftUI
import UIKit
import XCTest
@testable import Magician

/// The composer stack's pure rules: the queue row title, the auto-send
/// countdown, presence, the ask dock's items / scoping / switcher, and the
/// planner dock's selection.
final class ComposerStackTests: XCTestCase {

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
        readOnly: Bool = false
    ) -> [ComposerAskItem] {
        ComposerAskModel.items(
            chatEscalations: chat, attention: attention, sessionId: "session-1", threadId: "general",
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

    func testFeedAsksAreScopedToThisConversation() {
        let list = items(chat: [], attention: [Self.planApproval, Self.sessionScopedAsk, Self.unrelatedAsk])
        XCTAssertEqual(list.map(\.id), ["plan-1", "pause-scope"])
        guard case .choices(let plan) = list[0].form else { return XCTFail("plan approval → choices") }
        XCTAssertEqual(plan.map(\.label), ["Review plan", "Reject", "Approve", "Approve & Run"])
        XCTAssertEqual(list[0].taskId, "task-9")
        XCTAssertEqual(list[1].from, "from researcher")
    }

    func testPlanApprovalTheplannerDockOffersIsLeftToIt() {
        let list = items(chat: [], attention: [Self.planApproval], plannerTaskIds: ["task-9"])
        XCTAssertTrue(list.isEmpty)
    }

    func testChatCardAndFeedRowForOneAskShowOnce() {
        let list = items(chat: [Self.textCard], attention: [Self.duplicateOfChat])
        XCTAssertEqual(list.map(\.id), ["pause-2"])
        guard case .chat = list[0].source else { return XCTFail("the chat card wins") }
    }

    func testArchivedConversationAsksNothing() {
        XCTAssertTrue(items(attention: [Self.planApproval], readOnly: true).isEmpty)
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
     "task_id":"task-relentless","ui_thread_id":"general",
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
                         chatTaskIds: [])
        XCTAssertEqual(list.map(\.id), ["exec-9:relentless-review:run-7:hash", "pause-sibling"],
                       "the row's ui_thread_id places it here; its task's other asks follow")
        XCTAssertTrue(ComposerAskModel.belongs(Self.relentlessReview, sessionId: nil, threadId: "general",
                                               chatTaskIds: []))
        XCTAssertFalse(ComposerAskModel.belongs(Self.otherThreadAsk, sessionId: nil, threadId: "general",
                                                chatTaskIds: []))
        XCTAssertTrue(ComposerAskModel.belongs(Self.siblingOfRelentless, sessionId: nil, threadId: "general",
                                               chatTaskIds: ["task-relentless"]))
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
