import UserNotifications
import XCTest
@testable import Magician

final class AttentionPushAnswerTests: XCTestCase {
    // MARK: fixtures

    private func envelope(
        _ mode: String,
        correlation: String = "corr-1",
        source: String? = "agentic",
        optionCount: Any? = nil
    ) -> AttentionPushEnvelope {
        var info: [AnyHashable: Any] = [
            "kind": "attention_requested",
            "correlation_id": correlation,
            "answer_mode": mode,
            "deep_link": "magican://attention?correlation_id=\(correlation)",
        ]
        if let source { info["source"] = source }
        if let optionCount { info["option_count"] = optionCount }
        return AttentionPushEnvelope(userInfo: info)!
    }

    private func item(
        inputType: String,
        schema: [String: Any] = [:],
        metadata extra: [String: Any] = [:],
        itemType: String = "escalation",
        status: String = "needs_action",
        nestUnderHitl: Bool = false
    ) -> [String: Any] {
        var schema = schema
        schema["type"] = inputType
        var metadata: [String: Any] = ["source": "agentic", "input_type": inputType, "execution_id": "exec-9"]
        if nestUnderHitl {
            metadata["hitl_request"] = ["input_schema": schema, "scope": ["execution_id": "child-exec"]]
        } else {
            metadata["input_schema"] = schema
        }
        metadata.merge(extra) { _, new in new }
        return [
            "id": "item-1",
            "title": "Deploy to staging?",
            "summary": "The build is green.",
            "item_type": itemType,
            "status": status,
            "metadata": metadata,
            "task_id": "task-7",
        ]
    }

    private func ask(_ item: [String: Any]) -> AttentionPushAsk {
        AttentionPushAsk(json: try! JSONSerialization.data(withJSONObject: item))!
    }

    private func options(_ count: Int, requiresInput: Bool = false) -> [[String: Any]] {
        (0..<count).map { ["id": "o\($0)", "label": "Option \($0)", "requires_input": requiresInput] }
    }

    // MARK: envelope

    func testEnvelopeRequiresAttentionKindCorrelationAndKnownMode() {
        XCTAssertNil(AttentionPushEnvelope(userInfo: ["kind": "attention_requested", "correlation_id": "c"]))
        XCTAssertNil(AttentionPushEnvelope(userInfo: ["kind": "attention_resolved", "correlation_id": "c", "answer_mode": "confirm"]))
        XCTAssertNil(AttentionPushEnvelope(userInfo: ["kind": "attention_requested", "correlation_id": " ", "answer_mode": "confirm"]))
        XCTAssertNil(AttentionPushEnvelope(userInfo: ["kind": "attention_requested", "correlation_id": "c", "answer_mode": "form"]))
        let parsed = envelope("choice", optionCount: NSNumber(value: 3))
        XCTAssertEqual(parsed.mode, .choice)
        XCTAssertEqual(parsed.optionCount, 3)
        XCTAssertEqual(parsed.source, "agentic")
    }

    // MARK: eligibility

    func testConfirmEligibleOnlyForBenignConfirmation() {
        let benign = ask(item(inputType: "confirmation", schema: ["confirm_label": "Ship it", "deny_label": "Hold"]))
        let plan = AttentionPushEligibility.plan(for: benign, envelope: envelope("confirm"))
        XCTAssertEqual(plan?.confirmLabel, "Ship it")
        XCTAssertEqual(plan?.denyLabel, "Hold")
        XCTAssertEqual(plan?.question, "Deploy to staging?")
        XCTAssertEqual(plan?.executionID, "exec-9")

        let defaults = AttentionPushEligibility.plan(for: ask(item(inputType: "confirmation")), envelope: envelope("confirm"))
        XCTAssertEqual(defaults?.confirmLabel, "Approve")
        XCTAssertEqual(defaults?.denyLabel, "Reject")

        XCTAssertNil(AttentionPushEligibility.plan(
            for: ask(item(inputType: "confirmation", schema: ["destructive": true])), envelope: envelope("confirm")))
        XCTAssertNil(AttentionPushEligibility.plan(
            for: ask(item(inputType: "confirmation", schema: ["sensitive": ["kind": "password"]])), envelope: envelope("confirm")))
        XCTAssertNil(AttentionPushEligibility.plan(
            for: ask(item(inputType: "confirmation", schema: ["request_type": "secure_browser_confirm"])), envelope: envelope("confirm")))
        // Mode must agree with the content.
        XCTAssertNil(AttentionPushEligibility.plan(for: ask(item(inputType: "choice", schema: ["options": options(2)])),
                                                   envelope: envelope("confirm")))
    }

    func testChoiceEligibilityBounds() {
        for count in 1...4 {
            let plan = AttentionPushEligibility.plan(
                for: ask(item(inputType: "choice", schema: ["options": options(count)])),
                envelope: envelope("choice", optionCount: count))
            XCTAssertEqual(plan?.choices.count, count)
        }
        XCTAssertNil(AttentionPushEligibility.plan(
            for: ask(item(inputType: "choice", schema: ["options": options(0)])), envelope: envelope("choice")))
        XCTAssertNil(AttentionPushEligibility.plan(
            for: ask(item(inputType: "choice", schema: ["options": options(5)])), envelope: envelope("choice")))
        XCTAssertNil(AttentionPushEligibility.plan(
            for: ask(item(inputType: "choice", schema: ["options": options(2, requiresInput: true)])),
            envelope: envelope("choice")))
        XCTAssertNil(AttentionPushEligibility.plan(
            for: ask(item(inputType: "choice", schema: ["options": options(2), "multiple": true])),
            envelope: envelope("choice")))
        XCTAssertNil(AttentionPushEligibility.plan(
            for: ask(item(inputType: "multi_choice", schema: ["options": options(2)])), envelope: envelope("choice")))
        XCTAssertNil(AttentionPushEligibility.plan(
            for: ask(item(inputType: "choice", schema: ["options": options(2), "sensitive": ["kind": "otp"]])),
            envelope: envelope("choice")))
        // The option count the backend announced must still hold.
        XCTAssertNil(AttentionPushEligibility.plan(
            for: ask(item(inputType: "choice", schema: ["options": options(3)])),
            envelope: envelope("choice", optionCount: 2)))
        // A server-authored option operation other than a plain choice is app-only.
        let acted: [[String: Any]] = [["id": "a", "label": "A", "action": ["type": "continue_execution"]]]
        XCTAssertNil(AttentionPushEligibility.plan(
            for: ask(item(inputType: "choice", schema: ["options": acted])), envelope: envelope("choice")))
    }

    func testDestructiveIsOpenOnlyInEveryMode() {
        XCTAssertNil(AttentionPushEligibility.plan(
            for: ask(item(inputType: "choice", schema: ["options": options(2), "destructive": true])),
            envelope: envelope("choice")))
        XCTAssertNil(AttentionPushEligibility.plan(
            for: ask(item(inputType: "text", schema: ["destructive": true])), envelope: envelope("text")))
    }

    func testAllowOtherChoiceStaysAnswerableWithoutOther() {
        let plan = AttentionPushEligibility.plan(
            for: ask(item(inputType: "choice", schema: ["options": options(2), "allow_other": true])),
            envelope: envelope("choice"))
        XCTAssertEqual(plan?.choices.map(\.id), ["o0", "o1"])
    }

    func testFetchedItemMayBeWrapped() throws {
        let raw = item(inputType: "text")
        let wrapped = try XCTUnwrap(AttentionPushAsk(json: JSONSerialization.data(withJSONObject: ["item": raw])))
        XCTAssertEqual(wrapped, ask(raw))
        XCTAssertEqual(wrapped.question, "Deploy to staging?")
    }

    func testTextEligibility() {
        let plan = AttentionPushEligibility.plan(
            for: ask(item(inputType: "text", schema: ["placeholder": "Branch name"])), envelope: envelope("text"))
        XCTAssertEqual(plan?.mode, .text)
        XCTAssertEqual(plan?.placeholder, "Branch name")
        for blocked in ["password", "otp"] {
            XCTAssertNil(AttentionPushEligibility.plan(for: ask(item(inputType: blocked)), envelope: envelope("text")))
        }
        XCTAssertNil(AttentionPushEligibility.plan(
            for: ask(item(inputType: "text", schema: ["sensitive": ["kind": "password"]])), envelope: envelope("text")))
        XCTAssertNil(AttentionPushEligibility.plan(
            for: ask(item(inputType: "text", schema: ["questions": [["id": "q1", "prompt": "Name?"]]])),
            envelope: envelope("text")))
    }

    func testIneligibleRowStates() {
        XCTAssertNil(AttentionPushEligibility.plan(
            for: ask(item(inputType: "text", status: "done")), envelope: envelope("text")))
        XCTAssertNil(AttentionPushEligibility.plan(
            for: ask(item(inputType: "text", itemType: "failed")), envelope: envelope("text")))
        XCTAssertNil(AttentionPushEligibility.plan(
            for: ask(item(inputType: "text", metadata: ["attention_reason": "waiting_without_question"])),
            envelope: envelope("text")))
    }

    func testNestedHitlSchemaAndChildExecutionRouteLikeInApp() throws {
        let raw = item(inputType: "choice", schema: ["options": options(2)], nestUnderHitl: true)
        let pushAsk = ask(raw)
        let plan = try XCTUnwrap(AttentionPushEligibility.plan(for: pushAsk, envelope: envelope("choice")))
        XCTAssertEqual(plan.choices.map(\.id), ["o0", "o1"])
        // Parity with the in-app decoder the Attention tab answers through.
        let inApp = try JSONDecoder().decode(AttentionItem.self, from: JSONSerialization.data(withJSONObject: raw))
        XCTAssertEqual(pushAsk.inputType, inApp.inputType)
        XCTAssertEqual(pushAsk.source, inApp.source)
        XCTAssertEqual(pushAsk.options.map(\.id), inApp.options.map(\.id))
        XCTAssertEqual(pushAsk.executionID, inApp.responseExecutionId)
        XCTAssertEqual(plan.executionID, "child-exec")
    }

    func testApprovalItemsAreConfirmations() {
        let approval = ask(item(inputType: "text", itemType: "approval"))
        XCTAssertEqual(approval.source, "approval")
        XCTAssertEqual(approval.inputType, "confirmation")
        let plan = AttentionPushEligibility.plan(for: approval, envelope: envelope("confirm", source: nil))
        XCTAssertEqual(plan?.source, "approval")
    }

    // MARK: categories

    func testCategoryActionsPerMode() throws {
        let confirm = try XCTUnwrap(AttentionPushEligibility.plan(
            for: ask(item(inputType: "confirmation")), envelope: envelope("confirm")))
        let confirmCategory = AttentionPushCategories.category(for: confirm)
        XCTAssertEqual(confirmCategory.actions.map(\.identifier),
                       [AttentionPushAnswer.confirmAction, AttentionPushAnswer.denyAction])
        XCTAssertEqual(confirmCategory.actions.map(\.title), ["Approve", "Reject"])
        XCTAssertTrue(confirmCategory.actions.allSatisfy { $0.options.contains(.authenticationRequired) })
        XCTAssertFalse(confirmCategory.actions.contains { $0.options.contains(.foreground) })
        XCTAssertEqual(confirmCategory.hiddenPreviewsBodyPlaceholder, AttentionPushAnswer.hiddenPreviewsPlaceholder)
        XCTAssertTrue(confirmCategory.identifier.hasPrefix(AttentionPushAnswer.categoryPrefix))

        let choice = try XCTUnwrap(AttentionPushEligibility.plan(
            for: ask(item(inputType: "choice", schema: ["options": options(3)])), envelope: envelope("choice")))
        let choiceCategory = AttentionPushCategories.category(for: choice)
        XCTAssertEqual(choiceCategory.actions.map(\.identifier), ["opt:o0", "opt:o1", "opt:o2"])
        XCTAssertEqual(choiceCategory.actions.map(\.title), ["Option 0", "Option 1", "Option 2"])
        XCTAssertTrue(choiceCategory.actions.allSatisfy { $0.options.contains(.authenticationRequired) })

        let text = try XCTUnwrap(AttentionPushEligibility.plan(
            for: ask(item(inputType: "text")), envelope: envelope("text")))
        let textCategory = AttentionPushCategories.category(for: text)
        XCTAssertEqual(textCategory.actions.count, 1)
        XCTAssertTrue(textCategory.actions.first is UNTextInputNotificationAction)
        XCTAssertTrue(textCategory.actions[0].options.contains(.authenticationRequired))
    }

    func testCategoryIdentifierIsStableAndShort() {
        let a = AttentionPushCategories.identifier(correlationID: "pause/123 abc")
        XCTAssertEqual(a, AttentionPushCategories.identifier(correlationID: "pause/123 abc"))
        XCTAssertNotEqual(a, AttentionPushCategories.identifier(correlationID: "pause/124 abc"))
        XCTAssertEqual(a.count, AttentionPushAnswer.categoryPrefix.count + 12)
    }

    func testMergeKeepsOtherCategoriesAndPrunesStaleAsks() {
        let legacy = UNNotificationCategory(identifier: AttentionNotificationActions.category, actions: [],
                                            intentIdentifiers: [])
        let live = UNNotificationCategory(identifier: "MAGICAN_ASK_live", actions: [], intentIdentifiers: [])
        let stale = UNNotificationCategory(identifier: "MAGICAN_ASK_stale", actions: [], intentIdentifiers: [])
        let adding = UNNotificationCategory(identifier: "MAGICAN_ASK_new", actions: [], intentIdentifiers: [])
        let merged = AttentionPushCategories.merged(
            existing: [legacy, live, stale], adding: adding, inUse: ["MAGICAN_ASK_live"])
        XCTAssertEqual(Set(merged.map(\.identifier)),
                       [AttentionNotificationActions.category, "MAGICAN_ASK_live", "MAGICAN_ASK_new"])
    }

    // MARK: privacy

    func testPrivacyDecisionTable() {
        let rows: [(Bool, UNShowPreviewsSetting, Bool)] = [
            (false, .always, false),
            (false, .whenAuthenticated, true),
            (false, .never, false),
            (true, .always, true),
            (true, .whenAuthenticated, true),
            (true, .never, true),
        ]
        for (setting, previews, expected) in rows {
            XCTAssertEqual(AttentionPushPrivacy.revealsAsk(showOnLockScreen: setting, previews: previews), expected,
                           "setting \(setting) previews \(previews.rawValue)")
        }
    }

    func testLockScreenSettingDefaultsOff() {
        let suite = "attention-push-tests-\(UUID().uuidString)"
        let store = UserDefaults(suiteName: suite)!
        defer { store.removePersistentDomain(forName: suite) }
        XCTAssertFalse(AttentionPushAnswer.showQuestionsOnLockScreen(store))
        store.set(true, forKey: AttentionPushAnswer.showQuestionsOnLockScreenKey)
        XCTAssertTrue(AttentionPushAnswer.showQuestionsOnLockScreen(store))
    }

    func testEnrichedContentRevealsOnlyWhenAllowed() throws {
        let original = UNMutableNotificationContent()
        original.title = AttentionPushAnswer.genericTitle
        original.body = AttentionPushAnswer.genericBody
        original.userInfo = ["kind": "attention_requested", "correlation_id": "corr-1"]
        let plan = try XCTUnwrap(AttentionPushEligibility.plan(
            for: ask(item(inputType: "choice", schema: ["options": options(2)])), envelope: envelope("choice")))

        let hidden = AttentionPushContent.enriched(original, plan: plan, reveal: false)
        XCTAssertEqual(hidden.body, AttentionPushAnswer.genericBody)
        XCTAssertEqual(hidden.categoryIdentifier, "")
        XCTAssertNil(hidden.userInfo[AttentionPushAnswer.contextKey])

        let shown = AttentionPushContent.enriched(original, plan: plan, reveal: true)
        XCTAssertEqual(shown.body, "Deploy to staging?")
        XCTAssertEqual(shown.categoryIdentifier, AttentionPushCategories.identifier(correlationID: "corr-1"))
        let context = try XCTUnwrap(AttentionPushAnswerContext(userInfo: shown.userInfo))
        XCTAssertEqual(context, plan.context)
        XCTAssertEqual(context.optionIDs, ["o0", "o1"])
    }

    // MARK: value encoding

    private func context(_ mode: AttentionPushAnswerMode, source: String = "agentic",
                         inputType: String = "text", optionIDs: [String] = []) -> AttentionPushAnswerContext {
        AttentionPushAnswerContext(mode: mode, correlationID: "corr-1", source: source, inputType: inputType,
                                   executionID: "exec-9", taskID: "task-7", optionIDs: optionIDs)
    }

    func testValueEncodingPerMode() {
        let confirm = context(.confirm, inputType: "confirmation")
        XCTAssertEqual(AttentionPushAnswerAction.value(actionIdentifier: AttentionPushAnswer.confirmAction,
                                                       userText: nil, context: confirm) as NSDictionary?,
                       ["type": "confirmation", "confirmed": true])
        XCTAssertEqual(AttentionPushAnswerAction.value(actionIdentifier: AttentionPushAnswer.denyAction,
                                                       userText: nil, context: confirm) as NSDictionary?,
                       ["type": "confirmation", "confirmed": false])
        XCTAssertNil(AttentionPushAnswerAction.value(actionIdentifier: "opt:o0", userText: nil, context: confirm))

        let choice = context(.choice, inputType: "choice", optionIDs: ["o0", "o1"])
        XCTAssertEqual(AttentionPushAnswerAction.value(actionIdentifier: "opt:o1", userText: nil,
                                                       context: choice) as NSDictionary?,
                       ["type": "choice", "selected_id": "o1"])
        XCTAssertNil(AttentionPushAnswerAction.value(actionIdentifier: "opt:o9", userText: nil, context: choice))

        let text = context(.text)
        XCTAssertEqual(AttentionPushAnswerAction.value(actionIdentifier: AttentionPushAnswer.textAction,
                                                       userText: "  main \n", context: text) as NSDictionary?,
                       ["type": "text", "value": "main"])
        XCTAssertNil(AttentionPushAnswerAction.value(actionIdentifier: AttentionPushAnswer.textAction,
                                                     userText: "   ", context: text))

        XCTAssertTrue(AttentionPushAnswerAction.isAnswerAction("opt:x"))
        XCTAssertFalse(AttentionPushAnswerAction.isAnswerAction(AttentionNotificationActions.approve))
        XCTAssertFalse(AttentionPushAnswerAction.isAnswerAction(UNNotificationDefaultActionIdentifier))
    }

    func testRequestBodyCarriesChannelAndRouting() {
        let body = AttentionPushAnswerAction.requestBody(
            context: context(.choice, inputType: "choice", optionIDs: ["o0"]),
            value: ["type": "choice", "selected_id": "o0"])
        XCTAssertEqual(body["channel"] as? String, "ios_push")
        XCTAssertEqual(body["source"] as? String, "agentic")
        XCTAssertEqual(body["input_type"] as? String, "choice")
        XCTAssertEqual(body["execution_id"] as? String, "exec-9")
        XCTAssertNil(body["task_id"], "task_id rides only planning sources, like the in-app answer")

        let planning = AttentionPushAnswerAction.requestBody(
            context: context(.confirm, source: "plan_approval", inputType: "confirmation"),
            value: ["type": "confirmation", "confirmed": true])
        XCTAssertEqual(planning["task_id"] as? String, "task-7")
    }

    func testContextRoundTripsThroughUserInfo() {
        let original = context(.choice, inputType: "choice", optionIDs: ["a", "b"])
        let info: [AnyHashable: Any] = [AttentionPushAnswer.contextKey: original.userInfoValue]
        XCTAssertEqual(AttentionPushAnswerContext(userInfo: info), original)
        XCTAssertNil(AttentionPushAnswerContext(userInfo: [:]))
    }

    // MARK: result mapping

    func testResultMapping() {
        func json(_ object: [String: Any]) -> Data { try! JSONSerialization.data(withJSONObject: object) }
        XCTAssertEqual(AttentionPushAnswerResult.from(statusCode: 200, body: json(["accepted": true])), .sent)
        XCTAssertEqual(AttentionPushAnswerResult.from(statusCode: 204, body: nil), .sent)
        XCTAssertEqual(AttentionPushAnswerResult.from(statusCode: 409, body: json(["reason": "already_resolved"])),
                       .alreadyAnswered)
        XCTAssertEqual(AttentionPushAnswerResult.from(statusCode: 409, body: nil), .alreadyAnswered)
        XCTAssertEqual(AttentionPushAnswerResult.from(statusCode: 404, body: nil), .alreadyAnswered)
        XCTAssertEqual(AttentionPushAnswerResult.from(statusCode: 500, body: nil), .failed)
        XCTAssertEqual(AttentionPushAnswerResult.from(statusCode: 401, body: nil), .failed)
        XCTAssertEqual(AttentionPushAnswerResult.from(statusCode: nil, body: nil), .failed)
        XCTAssertEqual(AttentionPushAnswerResult.from(statusCode: 200, body: json(["status": "reask_required"])),
                       .failed)
        XCTAssertEqual(AttentionPushAnswerResult.from(statusCode: 200, body: json(["accepted": false])), .failed)
    }

    func testResultContentNeverCarriesTheQuestionOrButtons() {
        let original = UNMutableNotificationContent()
        original.body = "Deploy to staging?"
        original.threadIdentifier = "magican-attention"
        original.categoryIdentifier = "MAGICAN_ASK_abc"
        original.userInfo = [
            "correlation_id": "corr-1",
            "deep_link": "magican://attention?correlation_id=corr-1",
            AttentionPushAnswer.contextKey: context(.text).userInfoValue,
        ]
        for result in [AttentionPushAnswerResult.sent, .alreadyAnswered, .failed] {
            let content = AttentionPushAnswerHandler.resultContent(for: result, replacing: original)
            XCTAssertEqual(content.title, result.title)
            XCTAssertFalse(content.body.contains("staging"))
            XCTAssertEqual(content.categoryIdentifier, "")
            XCTAssertEqual(content.threadIdentifier, "magican-attention")
            XCTAssertEqual(content.userInfo["deep_link"] as? String, "magican://attention?correlation_id=corr-1")
            XCTAssertNil(content.userInfo[AttentionPushAnswer.contextKey])
        }
        XCTAssertEqual(AttentionPushAnswerResult.failed.title, "Couldn't send your answer — tap to open")
    }

    // MARK: settings hint

    func testPreviewHintDecision() {
        XCTAssertEqual(AttentionPushPreviewHint.hint(showOnLockScreen: false, previews: .always), .previewsAlways)
        XCTAssertEqual(AttentionPushPreviewHint.hint(showOnLockScreen: false, previews: .never), .previewsNever)
        XCTAssertNil(AttentionPushPreviewHint.hint(showOnLockScreen: false, previews: .whenAuthenticated))
        for previews in [UNShowPreviewsSetting.always, .never, .whenAuthenticated] {
            XCTAssertNil(AttentionPushPreviewHint.hint(showOnLockScreen: true, previews: previews))
        }
        XCTAssertTrue(AttentionPushPreviewHint.previewsAlways.message.contains("When Unlocked"))
        XCTAssertTrue(AttentionPushPreviewHint.previewsAlways.message.hasPrefix("iPhone shows Magican previews Always"))
    }

    // MARK: diagnostics

    func testEligibilityReasonsAreContentFreeCodes() {
        XCTAssertEqual(AttentionPushEligibility.evaluate(
            ask(item(inputType: "text", schema: ["destructive": true])), envelope: envelope("text")).reason,
            "destructive")
        XCTAssertEqual(AttentionPushEligibility.evaluate(
            ask(item(inputType: "choice", schema: ["options": options(3)])),
            envelope: envelope("choice", optionCount: 2)).reason, "option_count_mismatch")
        XCTAssertEqual(AttentionPushEligibility.evaluate(
            ask(item(inputType: "text")), envelope: envelope("confirm")).reason, "mode_mismatch")
        XCTAssertEqual(AttentionPushEligibility.evaluate(
            ask(item(inputType: "text")), envelope: envelope("text")).reason, "eligible")
    }

    func testPrivacyDecisionLabels() {
        XCTAssertEqual(AttentionPushPrivacy.decisionLabel(showOnLockScreen: false, previews: .always),
                       "hidden:previews_always")
        XCTAssertEqual(AttentionPushPrivacy.decisionLabel(showOnLockScreen: false, previews: .whenAuthenticated),
                       "reveal:previews_when_unlocked")
        XCTAssertEqual(AttentionPushPrivacy.decisionLabel(showOnLockScreen: true, previews: .never),
                       "reveal:setting_on")
    }

    func testDecisionRecordsKeepTheLatestTwentyWithoutContent() {
        let suite = "attention-push-diag-\(UUID().uuidString)"
        let store = UserDefaults(suiteName: suite)!
        defer { store.removePersistentDomain(forName: suite) }
        let hash = AttentionPushDiagnostics.correlationHash("pause/secret-ish id")
        XCTAssertEqual(hash.count, 12)
        XCTAssertFalse(hash.contains("pause"))
        for index in 0..<25 {
            AttentionPushDiagnostics.record(.init(
                at: Date(timeIntervalSince1970: TimeInterval(index)), correlationHash: hash, answerMode: "choice",
                privacy: "reveal:setting_on", fetchStatus: 200, fetchLatencyMs: index, eligibility: "eligible",
                categoryRegistered: true, outcome: "enriched", fallbackReason: nil), store: store)
        }
        let records = AttentionPushDiagnostics.records(store)
        XCTAssertEqual(records.count, AttentionPushDiagnostics.maxRecords)
        XCTAssertEqual(records.first?.fetchLatencyMs, 5)
        XCTAssertEqual(records.last?.fetchLatencyMs, 24)
        let raw = String(data: store.data(forKey: AttentionPushDiagnostics.recordsKey)!, encoding: .utf8)!
        XCTAssertFalse(raw.contains("pause"))
    }

    func testActionKindNeverExposesOptionIDs() {
        XCTAssertEqual(AttentionPushAnswerHandler.actionKind("opt:deploy-prod"), "option")
        XCTAssertEqual(AttentionPushAnswerHandler.actionKind(AttentionPushAnswer.textAction), AttentionPushAnswer.textAction)
    }

    // MARK: networking seams

    func testFetchURLEscapesTheCorrelationAsOneSegment() {
        let url = AttentionPushAskClient.url(correlationID: "approval/id with spaces",
                                             base: URL(string: "https://example.invalid")!)
        XCTAssertEqual(url?.absoluteString,
                       "https://example.invalid/api/magician/v2/feed/attention/approval%2Fid%20with%20spaces")
    }
}
