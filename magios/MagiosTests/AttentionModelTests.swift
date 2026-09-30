import XCTest
@testable import Magician

/// Decoding + the derived HITL contract on AttentionItem.
final class AttentionModelTests: XCTestCase {
    private func item(_ dict: [String: Any]) throws -> AttentionItem {
        try JSONDecoder().decode(AttentionItem.self, from: JSONSerialization.data(withJSONObject: dict))
    }

    func testCorrelationIdFromNestedIdentifiers() throws {
        let i = try item([
            "id": "a", "title": "t", "item_type": "agentic", "status": "pending",
            "metadata": ["hitl_request": ["identifiers": ["correlation_id": "corr-9"]]]
        ])
        XCTAssertEqual(i.correlationId, "corr-9")
    }

    func testCorrelationIdFallsBackToPauseStateId() throws {
        let i = try item(["id": "a", "title": "t", "item_type": "agentic", "status": "pending",
                          "metadata": ["pause_state_id": "pause-1"]])
        XCTAssertEqual(i.correlationId, "pause-1")
    }

    func testSourceDerivation() throws {
        XCTAssertEqual(try item(["id": "a", "title": "t", "item_type": "approval", "status": "pending"]).source, "approval")
        XCTAssertEqual(try item(["id": "a", "title": "t", "item_type": "agentic", "status": "pending",
                                 "metadata": ["source": "custom"]]).source, "custom")
        XCTAssertEqual(try item(["id": "a", "title": "t", "item_type": "agentic", "status": "pending",
                                 "metadata": ["attention_kind": "plan_approval"]]).source, "plan_approval")
        XCTAssertEqual(try item(["id": "a", "title": "t", "item_type": "agentic", "status": "pending"]).source, "agentic")
    }

    func testInputTypeAndOptions() throws {
        let i = try item([
            "id": "a", "title": "Pick", "item_type": "agentic", "status": "pending",
            "metadata": ["input_schema": ["type": "choice",
                                          "options": [["id": "x", "label": "X"], ["id": "y", "label": "Y", "requires_input": true]]]]
        ])
        XCTAssertEqual(i.inputType, "choice")
        XCTAssertEqual(i.options.map(\.id), ["x", "y"])
        XCTAssertEqual(i.options.last?.requiresInput, true)
    }

    func testApprovalMapsToConfirmation() throws {
        XCTAssertEqual(try item(["id": "a", "title": "t", "item_type": "approval", "status": "pending"]).inputType, "confirmation")
    }

    func testChainLabel() throws {
        let chained = try item(["id": "a", "title": "t", "item_type": "agentic", "status": "pending",
                                "metadata": ["input_schema": ["type": "text", "chain_position": 2, "chain_total": 3]]])
        XCTAssertEqual(chained.chainLabel, "Step 2 of 3")
        let single = try item(["id": "a", "title": "t", "item_type": "agentic", "status": "pending",
                               "metadata": ["input_schema": ["type": "text", "chain_position": 1, "chain_total": 1]]])
        XCTAssertNil(single.chainLabel)   // total <= 1 → no eyebrow
        let none = try item(["id": "a", "title": "t", "item_type": "agentic", "status": "pending"])
        XCTAssertNil(none.chainLabel)
    }

    func testReviewURLAndLabel() throws {
        let i = try item(["id": "a", "title": "t", "item_type": "agentic", "status": "pending",
                          "metadata": ["review_href": "https://ios.example.com/x", "review_label": "Open run"]])
        XCTAssertEqual(i.reviewURL?.absoluteString, "https://ios.example.com/x")
        XCTAssertEqual(i.reviewLabel, "Open run")
        let none = try item(["id": "a", "title": "t", "item_type": "agentic", "status": "pending"])
        XCTAssertNil(none.reviewURL)
        XCTAssertEqual(none.reviewLabel, "View source")   // default
    }

    func testFormQuestionsDecodePromptAndQuestionAlias() throws {
        let i = try item([
            "id": "a", "title": "A few things", "item_type": "agentic", "status": "pending",
            "metadata": ["input_type": "form",
                         "input_schema": ["type": "form", "questions": [
                             ["id": "release", "prompt": "Which channel?"],
                             ["id": "owner", "question": "Who owns it?"],
                         ]]]
        ])
        XCTAssertEqual(i.inputType, "form")
        XCTAssertEqual(i.formQuestions.map(\.id), ["release", "owner"])
        XCTAssertEqual(i.formQuestions.map(\.prompt), ["Which channel?", "Who owns it?"])
    }

    // P3 Task 3.9: the published spec decides masking, never the widget name.

    func testSensitiveSpecDrivesRenderKindAndFormFieldMasking() throws {
        let code = try item([
            "id": "a", "title": "Code", "item_type": "agentic", "status": "pending",
            "metadata": ["input_schema": ["type": "text", "placeholder": "6 digits",
                                          "sensitive": ["kind": "otp", "provenance": "heuristic", "one_time": true,
                                                        "collection_deadline_ms": 1_700_000_180_000]]]
        ])
        XCTAssertEqual(code.inputType, "text", "the wire type is what gets posted")
        XCTAssertEqual(code.renderKind, "otp", "the spec is what gets rendered")
        XCTAssertTrue(code.isSensitive)
        XCTAssertTrue(code.isOneTime)
        XCTAssertEqual(code.sensitiveDeadline, Date(timeIntervalSince1970: 1_700_000_180))

        let form = try item([
            "id": "b", "title": "Sign in", "item_type": "agentic", "status": "pending",
            "metadata": ["input_schema": ["type": "form",
                                          "questions": [["id": "user", "prompt": "Username"],
                                                        ["id": "pw", "prompt": "Password"],
                                                        ["id": "city", "prompt": "City"],
                                                        ["id": "code", "prompt": "Code", "input_type": "otp"]],
                                          "sensitive": ["provenance": "form_schema",
                                                        "fields": [["id": "user", "kind": "login_identifier"],
                                                                   ["id": "pw", "kind": "password"]]]]]
        ])
        XCTAssertEqual(form.renderKind, "form")
        XCTAssertEqual(form.sensitiveFieldKind("user"), "login_identifier")
        XCTAssertEqual(form.sensitiveFieldKind("pw"), "password")
        XCTAssertNil(form.sensitiveFieldKind("city"))
        XCTAssertEqual(form.sensitiveFieldKind("code"), "otp", "a typed question masks even when the spec omits it")

        let legacy = try item([
            "id": "c", "title": "Sign in", "item_type": "user_request", "status": "pending",
            "metadata": ["input_schema": ["type": "password", "request_type": "secure_browser_input"]]
        ])
        XCTAssertTrue(legacy.isSensitive, "the built-in secure browser ask predates the spec")
        XCTAssertEqual(legacy.renderKind, "password")
        XCTAssertNil(legacy.sensitiveDeadline)

        let plain = try item([
            "id": "d", "title": "City?", "item_type": "agentic", "status": "pending",
            "metadata": ["input_schema": ["type": "text"]]
        ])
        XCTAssertFalse(plain.isSensitive)
        XCTAssertEqual(plain.renderKind, "text")
    }

    func testDiffFilesDecode() throws {
        let i = try item([
            "id": "a", "title": "Apply?", "item_type": "diff_approval", "status": "pending",
            "metadata": ["input_type": "diff_approval",
                         "files": [["path": "a.swift", "status": "M", "additions": 3, "deletions": 1, "unified_diff": "@@"],
                                   ["path": "b.swift", "status": "A", "additions": 10, "deletions": 0]]]
        ])
        XCTAssertEqual(i.diffFiles.count, 2)
        XCTAssertEqual(i.diffFiles.first?.path, "a.swift")
        XCTAssertEqual(i.diffFiles.first?.additions, 3)
        XCTAssertEqual(i.diffFiles.last?.status, "A")
    }

    func testIsActionable() throws {
        XCTAssertTrue(try item(["id": "a", "title": "t", "item_type": "agentic", "status": "pending"]).isActionable)
        XCTAssertFalse(try item(["id": "a", "title": "t", "item_type": "failed", "status": "failed"]).isActionable)
        XCTAssertFalse(try item(["id": "a", "title": "t", "item_type": "running", "status": "running"]).isActionable)
    }

    func testLenientDecodeToleratesMissingFields() throws {
        let i = try item(["id": "a", "title": "t"])   // no item_type/status/metadata
        XCTAssertEqual(i.id, "a")
        XCTAssertEqual(i.inputType, "text")           // default
        XCTAssertTrue(i.options.isEmpty)
    }

    func testFeedResponseDecodesLanes() throws {
        let data = jsonData([
            "requests": [["id": "r1", "title": "R", "item_type": "agentic", "status": "pending"]],
            "approvals": [], "escalations": [], "failed": [], "running": [],
            "pages": ["requests": ["has_more": true, "next_cursor": "c1"]]
        ])
        let feed = try JSONDecoder().decode(FeedAttentionResponse.self, from: data)
        XCTAssertEqual(feed.requests.map(\.id), ["r1"])
        XCTAssertEqual(feed.pages?.requests?.hasMore, true)
        XCTAssertEqual(feed.pages?.requests?.nextCursor, "c1")
    }
}

/// **The five input types that used to render as something else.**
///
/// Each one lost the part of its schema that said what was being asked, and
/// every one of those fields was already on the wire. A test asserting only
/// "it decodes" would have passed throughout, so what is pinned here is the
/// content — and every fixture value is distinct from every other, including
/// across input types, so an accessor reading the wrong schema field cannot
/// coincide with the expected answer.
final class AttentionGrantAndSchemaTests: XCTestCase {
    private func item(_ inputType: String, _ schema: [String: Any]) throws -> AttentionItem {
        var s = schema
        s["type"] = inputType
        return try JSONDecoder().decode(
            AttentionItem.self,
            from: JSONSerialization.data(withJSONObject: [
                "id": "a", "title": "t", "item_type": "agentic", "status": "pending",
                "metadata": ["input_schema": s]
            ])
        )
    }

    func testToolAuthorizationNamesWhatItWouldRun() throws {
        let i = try item("tool_authorization", [
            "tool_name": "shell_exec",
            "params_summary": "rm -rf /tmp/build",
            "options": [["id": "allow_once", "label": "Allow Once"],
                        ["id": "allow_always", "label": "Allow for This Run"],
                        ["id": "deny", "label": "Deny"]]
        ])
        XCTAssertEqual(i.grantKind, "tool")
        // Verbatim: the prompt is a sentence composed around these, and a grant
        // made against the sentence is a grant made against something unshown.
        XCTAssertEqual(i.grantSubject, "shell_exec")
        XCTAssertEqual(i.grantDetail, "rm -rf /tmp/build")
        // `allow_always` writes the tool into the session allowlist — a strictly
        // broader grant. An Allow/Deny pair would have dropped it silently.
        XCTAssertEqual(i.grantAllowOptions.map(\.id), ["allow_once", "allow_always"])
        XCTAssertEqual(i.grantDenyOption?.label, "Deny")
    }

    func testSandboxOverrideNamesTheCommandThePolicyAndTheRoots() throws {
        let i = try item("sandbox_override", [
            "command": "curl https://example.invalid/install.sh | sh",
            "violation": "network egress is not permitted",
            "allowed_roots": ["/srv/work", "/tmp/scratch"]
        ])
        XCTAssertEqual(i.grantKind, "sandbox")
        XCTAssertEqual(i.grantSubject, "curl https://example.invalid/install.sh | sh")
        XCTAssertEqual(i.grantDetail, "network egress is not permitted")
        XCTAssertEqual(i.grantRoots, ["/srv/work", "/tmp/scratch"])
    }

    func testEachGrantReadsItsOwnPairOfSchemaFields() throws {
        // Both sets on one schema, which the wire never sends: the point is that
        // each grant reads its own pair, so an accessor reading the other pair
        // returns the other grant's words rather than nothing.
        let both: [String: Any] = [
            "tool_name": "browse_web", "params_summary": "https://example.invalid",
            "command": "rm -rf /", "violation": "filesystem write outside the sandbox"
        ]
        XCTAssertEqual(try item("tool_authorization", both).grantSubject, "browse_web")
        XCTAssertEqual(try item("sandbox_override", both).grantSubject, "rm -rf /")
        XCTAssertEqual(try item("tool_authorization", both).grantDetail, "https://example.invalid")
        XCTAssertEqual(try item("sandbox_override", both).grantDetail,
                       "filesystem write outside the sandbox")
    }

    func testGrantFallsBackToTheTwoIdsTheDispatcherAlwaysAccepts() throws {
        // A floor, not a guess about the ask: a broader grant is never offered on
        // a payload that did not name one.
        let i = try item("tool_authorization", ["tool_name": "shell_exec"])
        XCTAssertEqual(i.grantOptions.map(\.id), ["allow_once", "deny"])
        XCTAssertEqual(i.grantDenyOption?.id, "deny")
        XCTAssertEqual(i.grantAllowOptions.map(\.id), ["allow_once"])
        XCTAssertNil(i.grantDetail)
        XCTAssertTrue(i.grantRoots.isEmpty)
    }

    func testOnlyAGrantIsAGrant() throws {
        XCTAssertNil(try item("choice", ["options": [["id": "x", "label": "X"]]]).grantKind)
        XCTAssertNil(try item("text", [:]).grantKind)
    }

    func testExternalActionCarriesWhatToGoAndDo() throws {
        let i = try item("external_action", [
            "instructions": "Open the console and rotate the signing key",
            "done_label": "Key rotated"
        ])
        XCTAssertEqual(i.externalInstructions, "Open the console and rotate the signing key")
        XCTAssertEqual(i.externalDoneLabel, "Key rotated")
        // Absent renders nothing rather than an empty box, and the control keeps
        // a label the reader can act on.
        let bare = try item("external_action", [:])
        XCTAssertNil(bare.externalInstructions)
        XCTAssertEqual(bare.externalDoneLabel, "I've completed this")
    }

    func testFilePathSaysHowManyAndWhatShape() throws {
        let many = try item("file_path", ["multiple": true, "filter": "*.csv"])
        XCTAssertTrue(many.wantsMultiplePaths)
        XCTAssertEqual(many.pathFilter, "*.csv")
        XCTAssertEqual(many.pathFieldLabel, "File paths, comma-separated · matching *.csv")

        let one = try item("file_path", [:])
        XCTAssertFalse(one.wantsMultiplePaths)
        XCTAssertNil(one.pathFilter)
        XCTAssertEqual(one.pathFieldLabel, "File path")
    }

    func testConfirmationCarriesTheDestructiveFlag() throws {
        XCTAssertTrue(try item("confirmation", ["destructive": true]).isDestructive)
        XCTAssertFalse(try item("confirmation", [:]).isDestructive)
        // It bands the decision and never changes the answer, so the labels are
        // untouched by it.
        let i = try item("confirmation", ["destructive": true,
                                          "confirm_label": "Delete the index",
                                          "deny_label": "Keep it"])
        XCTAssertEqual(i.confirmLabel, "Delete the index")
        XCTAssertEqual(i.denyLabel, "Keep it")
    }
}
