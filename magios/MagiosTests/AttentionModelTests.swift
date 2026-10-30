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

    // MARK: - HITL surfacing fixes (C2–C4)

    private func row(_ dict: [String: Any]) throws -> AttentionItem {
        try JSONDecoder().decode(AttentionItem.self, from: JSONSerialization.data(withJSONObject: dict))
    }

    func testDelegatedChildAskRoutesToTheChildExecution() throws {
        let i = try row([
            "id": "a", "title": "Which landlord?", "item_type": "request", "status": "needs_action",
            "task_id": "parent-task", "execution_id": "parent-exec",
            "metadata": [
                "delegation_child": true, "delegated_agent_id": "researcher",
                "hitl_request": ["correlation_id": "child-pause", "source": "agentic",
                                 "execution_id": "child-exec", "prompt": "Which landlord?",
                                 "input_type": "text", "input_schema": ["type": "text"]]
            ]
        ])
        XCTAssertTrue(i.isDelegatedChildAsk)
        XCTAssertEqual(i.delegatedFromLabel, "from researcher")
        XCTAssertEqual(i.correlationId, "child-pause")
        XCTAssertEqual(i.responseExecutionId, "child-exec")
        XCTAssertEqual(i.owningTaskId, "parent-task")
        XCTAssertEqual(i.source, "agentic")
        XCTAssertEqual(i.prompt, "Which landlord?")
    }

    /// The backend's shipped C2 shape: ids under `identifiers`, the child's
    /// run under `hitl_request.scope`, parent task at the top level.
    func testBackendChildAskShapeRoutesViaScope() throws {
        let i = try row([
            "id": "v3:attention:x", "title": "Input requested", "item_type": "task", "status": "needs_action",
            "task_id": "parent-task",
            "metadata": [
                "delegation_child": true, "delegated_agent_id": "executive-assistant",
                "hitl_request": ["id": "child-pause", "source": "agentic", "input_type": "text",
                                 "identifiers": ["correlation_id": "child-pause"],
                                 "scope": ["task_id": "parent-task", "execution_id": "child-exec",
                                           "agent_id": "personal-assistant"]]
            ]
        ])
        XCTAssertEqual(i.correlationId, "child-pause")
        XCTAssertEqual(i.responseExecutionId, "child-exec")
        XCTAssertEqual(i.owningTaskId, "parent-task")
    }

    func testDelegatedFormAskReadsQuestionsFromHitlRequestSchema() throws {
        let i = try row([
            "id": "a", "title": "t", "item_type": "request", "status": "needs_action",
            "metadata": [
                "delegation_child": true,
                "hitl_request": ["correlation_id": "c", "input_type": "form", "input_schema": [
                    "type": "form",
                    "questions": [
                        ["id": "q1", "prompt": "Name?", "input_type": "text"],
                        ["id": "q2", "prompt": "Tone?", "input_type": "choice",
                         "options": [["id": "warm", "label": "Warm"], ["id": "firm", "label": "Firm"]]],
                        ["id": "q3", "prompt": "Channels?", "input_type": "multi_choice",
                         "options": [["id": "a", "label": "A"], ["id": "b", "label": "B"]]],
                        ["id": "q4", "prompt": "Skip me", "input_type": "text"],
                    ]]]
            ]
        ])
        XCTAssertEqual(i.inputType, "form")
        XCTAssertEqual(i.formQuestions.map { $0.id }, ["q1", "q2", "q3", "q4"])
        XCTAssertEqual(i.delegatedFromLabel, "from a delegated agent")
        let answers = i.formAnswers(values: ["q1": "Ada"],
                                    selections: ["q2": ["firm"], "q3": ["b", "a"]],
                                    skipped: ["q4"])
        XCTAssertEqual(answers.count, 4)
        XCTAssertEqual(answers[0]["value"] as? String, "Ada")
        XCTAssertNil(answers[0]["selected_ids"])
        XCTAssertEqual(answers[1]["selected_ids"] as? [String], ["firm"])
        XCTAssertEqual(answers[2]["selected_ids"] as? [String], ["a", "b"], "options' own order")
        XCTAssertEqual(answers[3]["skipped"] as? Bool, true)
        XCTAssertNil(answers[3]["value"])
    }

    func testStalledWaitIsRecognisedWithDefaultActions() throws {
        let i = try row([
            "id": "stall-1", "title": "Waiting on you, but no question was delivered",
            "item_type": "escalation", "status": "needs_action",
            "metadata": ["attention_reason": "waiting_without_question",
                         "execution_id": "exec-7", "task_id": "task-7"]
        ])
        XCTAssertTrue(i.isStalledWait)
        XCTAssertTrue(i.isActionable)
        XCTAssertEqual(i.stalledWaitActions, ["cancel", "open_task"])
        XCTAssertEqual(i.responseExecutionId, "exec-7")
        XCTAssertEqual(i.owningTaskId, "task-7")
        XCTAssertEqual(i.rowKindLabel, "stalled")
    }

    func testMcpOAuthRendersAsExternalActionWithItsURL() throws {
        let i = try row([
            "id": "o", "title": "Connect Linear", "item_type": "request", "status": "needs_action",
            "metadata": ["source": "mcp_oauth",
                         "hitl_request": ["correlation_id": "oauth-1", "source": "mcp_oauth",
                                          "input_schema": ["url": "https://auth.example.com/authorize?x=1"]]]
        ])
        XCTAssertEqual(i.source, "mcp_oauth")
        XCTAssertEqual(i.inputType, "external_action")
        XCTAssertEqual(i.renderKind, "external_action")
        XCTAssertEqual(i.externalActionURL?.absoluteString, "https://auth.example.com/authorize?x=1")
        XCTAssertEqual(i.externalDoneLabel, "I've finished")
        XCTAssertEqual(i.correlationId, "oauth-1")
        // Every sign-in can be declined; older asks carry no label.
        XCTAssertEqual(i.externalDismissLabel, "Not now")
    }

    func testMcpOAuthCarriesItsDeclineAndServiceLabels() throws {
        let i = try row([
            "id": "o", "title": "Finish connecting Swiggy (food) in your browser.",
            "item_type": "request", "status": "needs_action",
            "metadata": ["source": "mcp_oauth",
                         "hitl_request": ["correlation_id": "oauth-2", "source": "mcp_oauth",
                                          "input_schema": ["dismiss_label": "Later",
                                                           "service_label": "Swiggy (food)",
                                                           "browser_signin": true]]]
        ])
        XCTAssertEqual(i.externalDismissLabel, "Later")
        XCTAssertEqual(i.schema?.serviceLabel, "Swiggy (food)")
        // Only a sign-in offers the decline.
        XCTAssertNil(try item("external_action", ["dismiss_label": "Not now"]).externalDismissLabel)
    }

    func testExternalActionIgnoresNonWebURL() throws {
        let i = try row([
            "id": "o", "title": "t", "item_type": "request", "status": "needs_action",
            "metadata": ["source": "mcp_oauth", "input_schema": ["url": "javascript:alert(1)"]]
        ])
        XCTAssertNil(i.externalActionURL)
    }

    func testUnknownSourceStillRendersAsAnswerableText() throws {
        let i = try row([
            "id": "u", "title": "Something new", "item_type": "request", "status": "needs_action",
            "metadata": ["source": "brand_new_source", "pause_state_id": "p-1",
                         "actions": "not-an-array"]
        ])
        XCTAssertEqual(i.source, "brand_new_source")
        XCTAssertEqual(i.inputType, "text")
        XCTAssertEqual(i.correlationId, "p-1", "one odd metadata field must not drop the rest")
        XCTAssertTrue(i.isActionable)
    }

    // MARK: approval previews (`input_schema.approval_preview`)

    private static let previewedConfirmation = #"""
    {"id":"feed-1","title":"Confirmation required","item_type":"agentic","status":"needs_action",
     "metadata":{"hitl_request":{"input_type":"confirmation","prompt":"Mira wants to change positioning.md (+2 lines, −1 lines)",
      "identifiers":{"correlation_id":"corr-1"},
      "input_schema":{"action_type":"propose_program_revision",
        "action_summary":"pack:propose_program_revision(content=...)","reason":"Tighten the pitch",
        "approval_preview":{"title":"Mira wants to change positioning.md","why_label":"Agent's reason","why":"**Tighten** the pitch",
          "diff":{"path":"positioning.md","added":2,"removed":1,"truncated":true,"omitted_changed_lines":7,
            "unified_diff":"--- a/positioning.md\n+++ b/positioning.md\n",
            "hunks":[{"old_start":3,"old_lines":3,"new_start":3,"new_lines":4,"lines":[
              {"kind":"context","text":"  keep indent"},{"kind":"remove","text":"old line"},
              {"kind":"add","text":"new line"},{"kind":"add","text":"another"},{"kind":"moved","text":"unknown kind"},
              {"kind":"context","text":"tail"}]}]},
          "evidence":["https://example.com/report",42,"Customer call notes"],
          "warnings":[{"code":"stale_base","message":"The document changed since the agent read it."},{"code":"broken"}],
          "notes":["Shared document"],"future_field":{"x":1}}}}}}
    """#

    func testApprovalPreviewDecodesDiffWarningsEvidenceAndRawCall() throws {
        let item = try JSONDecoder().decode(AttentionItem.self, from: Data(Self.previewedConfirmation.utf8))
        XCTAssertEqual(item.inputType, "confirmation")
        let preview = try XCTUnwrap(item.approvalPreview)
        XCTAssertEqual(preview.title, "Mira wants to change positioning.md")
        XCTAssertEqual(preview.whyLabel, "Agent's reason")
        XCTAssertEqual(preview.why, "**Tighten** the pitch", "agent text is kept verbatim")
        XCTAssertEqual(preview.warnings, [.init(code: "stale_base", message: "The document changed since the agent read it.")],
                       "a warning without a message is dropped, not the list")
        XCTAssertEqual(preview.evidence, ["https://example.com/report", "Customer call notes"])
        XCTAssertEqual(preview.notes, ["Shared document"])
        XCTAssertEqual(preview.changeLabel, "+2 lines, −1 lines")
        let diff = try XCTUnwrap(preview.diff)
        XCTAssertEqual(diff.path, "positioning.md")
        XCTAssertTrue(diff.truncated); XCTAssertEqual(diff.omittedChangedLines, 7)
        let hunk = try XCTUnwrap(diff.hunks.first)
        XCTAssertEqual(hunk.header, "@@ -3,3 +3,4 @@")
        XCTAssertEqual(hunk.lines.map(\.kind), [.context, .remove, .add, .add, .context], "an unknown line kind is skipped")
        XCTAssertEqual(hunk.lines.first?.text, "  keep indent")
        XCTAssertEqual(item.approvalRawCall, "pack:propose_program_revision(content=...)\n\nTighten the pitch")
        let rendered = ApprovalPreviewCard.unifiedDiff(diff)
        XCTAssertEqual(rendered.additions, 2); XCTAssertEqual(rendered.deletions, 1)
        XCTAssertEqual(rendered.hunks.first?.lines.map(\.oldNumber), [3, 4, nil, nil, 5])
        XCTAssertEqual(rendered.hunks.first?.lines.map(\.newNumber), [3, nil, 4, 5, 6])
        let ask = try XCTUnwrap(ComposerAskModel.item(attention: item))
        XCTAssertEqual(ask.form, .handoff("Review in Attention →"), "a previewed approval is decided where its diff shows")
    }

    func testMissingOrUnusablePreviewKeepsThePlainConfirmation() throws {
        func row(_ schema: String) throws -> AttentionItem {
            let json = #"{"id":"a","title":"Confirm","item_type":"agentic","status":"needs_action","metadata":{"input_schema":"# + schema + "}}"
            return try JSONDecoder().decode(AttentionItem.self, from: Data(json.utf8))
        }
        let plain = try row(#"{"type":"confirmation","action_summary":"Click delete","reason":"Cleanup"}"#)
        XCTAssertEqual(plain.inputType, "confirmation")
        XCTAssertNil(plain.approvalPreview)
        XCTAssertEqual(plain.approvalRawCall, "Click delete\n\nCleanup")
        if case .choices = try XCTUnwrap(ComposerAskModel.item(attention: plain)).form {} else { XCTFail("a plain confirmation stays answerable in place") }
        for odd in [#""a string""#, #"{"why":"no title"}"#, #"{"title":"  "}"#, "7"] {
            let odd = try row(#"{"type":"confirmation","confirm_label":"Go","approval_preview":"# + odd + "}")
            XCTAssertNil(odd.approvalPreview)
            XCTAssertEqual(odd.confirmLabel, "Go", "an odd preview never drops the rest of the schema")
        }
        let badDiff = try row(#"{"type":"confirmation","approval_preview":{"title":"Change","diff":{"added":1},"warnings":"x"}}"#)
        let preview = try XCTUnwrap(badDiff.approvalPreview)
        XCTAssertEqual(preview.title, "Change")
        XCTAssertNil(preview.diff, "a diff without a path is not shown")
        XCTAssertTrue(preview.warnings.isEmpty)
    }
    // MARK: outward-send review (`escalation_trigger == "outward_review"`)

    private static let outwardReviewAsk = #"""
    {"id":"feed-o","title":"Review send","item_type":"agentic","status":"needs_action",
     "metadata":{"hitl_request":{"source":"agentic","input_type":"confirmation",
      "prompt":"ambassador wants to send an email to a@b.com",
      "identifiers":{"correlation_id":"corr-o"},
      "input_schema":{"type":"confirmation","action_type":"gmail_send",
        "action_summary":"gmail:send(to=a@b.com)","reason":"Follow up on the call",
        "approval_preview":{"title":"Send an email to a@b.com","why_label":"Agent's reason","why":"Follow up",
          "evidence":["Call notes"],"warnings":[],"notes":["First message to this recipient"]},
        "escalation_trigger":"outward_review",
        "options":[{"id":"send","label":"Send"},{"id":"discard","label":"Discard"},
                   {"id":"always_allow_recipient","label":"Always allow ambassador to send to a@b.com"}],
        "outward_review":{"capability":"gmail","action":"send","class":"message",
          "recipients":["a@b.com"],"agent_id":"ambassador","deadline_at_ms":1893456000000,"future":true}}}}}
    """#

    func testOutwardReviewDecodesRecordOptionsAndPreview() throws {
        let item = try JSONDecoder().decode(AttentionItem.self, from: Data(Self.outwardReviewAsk.utf8))
        XCTAssertEqual(item.inputType, "confirmation")
        XCTAssertTrue(item.isOutwardReview)
        XCTAssertEqual(item.approvalPreview?.title, "Send an email to a@b.com", "the review shows its card")
        let review = try XCTUnwrap(item.outwardReview)
        XCTAssertEqual(review, OutwardReview(capability: "gmail", action: "send", sendClass: "message",
                                             recipients: ["a@b.com"], agentId: "ambassador",
                                             deadlineAtMs: 1_893_456_000_000))
        XCTAssertEqual(review.recipientLine, "To a@b.com · via gmail")
        XCTAssertEqual(review.deadline, Date(timeIntervalSince1970: 1_893_456_000))
        XCTAssertTrue(review.expiryLine(now: Date(timeIntervalSince1970: 0))?.hasPrefix("Expires ") == true)
        XCTAssertEqual(review.expiryLine(now: Date(timeIntervalSince1970: 1_893_456_001)),
                       "Expired — an unanswered send is discarded.")
        let choices = try XCTUnwrap(item.outwardReviewOptions)
        XCTAssertEqual(choices.send, AttentionGrantOption(id: "send", label: "Send"))
        XCTAssertEqual(choices.discard, AttentionGrantOption(id: "discard", label: "Discard"))
        XCTAssertEqual(choices.alwaysAllow,
                       AttentionGrantOption(id: "always_allow_recipient", label: "Always allow ambassador to send to a@b.com"),
                       "the remembered grant keeps the server's label")
        let ask = try XCTUnwrap(ComposerAskModel.item(attention: item))
        XCTAssertEqual(ask.form, .handoff("Review in Attention →"), "a send is reviewed where its card shows")
    }

    func testOutwardReviewDecodesLenientlyAndNeverInventsTheGrant() throws {
        func row(_ schema: String) throws -> AttentionItem {
            let json = #"{"id":"a","title":"Send?","item_type":"agentic","status":"needs_action","metadata":{"input_schema":"# + schema + "}}"
            return try JSONDecoder().decode(AttentionItem.self, from: Data(json.utf8))
        }
        // No record, no options, no card: still a review, with the two contract ids.
        let bare = try row(#"{"type":"confirmation","escalation_trigger":"outward_review","confirm_label":"Go"}"#)
        XCTAssertTrue(bare.isOutwardReview)
        XCTAssertNil(bare.outwardReview)
        XCTAssertNil(bare.approvalPreview)
        XCTAssertEqual(bare.confirmLabel, "Go")
        let floor = try XCTUnwrap(bare.outwardReviewOptions)
        XCTAssertEqual([floor.send.id, floor.discard.id], ["send", "discard"])
        XCTAssertNil(floor.alwaysAllow, "a broader grant is never offered on a payload that did not name it")
        XCTAssertEqual(try XCTUnwrap(ComposerAskModel.item(attention: bare)).form, .handoff("Review in Attention →"))

        // Odd shapes drop the field, never the schema.
        for odd in [#""a string""#, "7", #"{"recipients":"a@b.com","deadline_at_ms":"soon","agent_id":3}"#] {
            let item = try row(#"{"type":"confirmation","escalation_trigger":"outward_review","confirm_label":"Go","outward_review":"# + odd + "}")
            XCTAssertTrue(item.isOutwardReview)
            XCTAssertEqual(item.confirmLabel, "Go", "an odd record never drops the rest of the schema")
            let review = try XCTUnwrap(item.outwardReview)
            XCTAssertEqual(review.recipients, [])
            XCTAssertEqual(review.deadlineAtMs, 0)
            XCTAssertNil(review.expiryLine(), "no deadline, no expiry line")
            XCTAssertNil(review.agentId)
        }
        let mixed = try row(#"{"type":"confirmation","escalation_trigger":"outward_review","outward_review":{"recipients":["a@b.com",4," "],"deadline_at_ms":0}}"#)
        XCTAssertEqual(mixed.outwardReview?.recipients, ["a@b.com"])
        XCTAssertNil(mixed.outwardReview?.expiryLine(), "deadline 0 means none was stamped")
        XCTAssertEqual(mixed.outwardReview?.recipientLine, "To a@b.com")
    }

    func testOrdinaryConfirmationsAndHoldsAreNotOutwardReviews() throws {
        func row(_ metadata: [String: Any]) throws -> AttentionItem {
            let dict: [String: Any] = ["id": "a", "title": "t", "item_type": "agentic", "status": "needs_action", "metadata": metadata]
            return try JSONDecoder().decode(AttentionItem.self, from: JSONSerialization.data(withJSONObject: dict))
        }
        let plain = try row(["input_schema": ["type": "confirmation"]])
        XCTAssertFalse(plain.isOutwardReview)
        XCTAssertNil(plain.outwardReviewOptions)
        if case .choices = try XCTUnwrap(ComposerAskModel.item(attention: plain)).form {} else {
            XCTFail("a plain confirmation stays answerable in place")
        }
        let otherTrigger = try row(["input_schema": ["type": "confirmation", "escalation_trigger": "criticality",
                                                      "outward_review": ["recipients": ["a@b.com"]]]])
        XCTAssertFalse(otherTrigger.isOutwardReview)
        XCTAssertNil(otherTrigger.outwardReview, "the record is read only under its trigger")
        let notConfirmation = try row(["input_schema": ["type": "choice", "escalation_trigger": "outward_review"]])
        XCTAssertFalse(notConfirmation.isOutwardReview)
        let plan = try row(["source": "plan_approval",
                            "input_schema": ["type": "confirmation", "escalation_trigger": "outward_review"]])
        XCTAssertFalse(plan.isOutwardReview, "a plan approval keeps its own buttons")
    }
}

/// Settings → Service accounts rows stay decodable from engines before and
/// after the per-row status / per-endpoint fields.
final class BrowserServiceAccountDecodingTests: XCTestCase {
    func testDecodesStatusDetailAndEndpointAlias() {
        let rows = BrowserServiceAccount.parse([
            ["name": "metabase-mcp", "label": "Provider account", "status": "connected", "detail": "Account login is active"],
            ["name": "swiggy-mcp", "label": "Provider account — instamart", "status": "needs_sign_in",
             "detail": "Account login is required", "endpoint_alias": "instamart"],
        ])
        XCTAssertEqual(rows.count, 2)
        XCTAssertEqual(rows[0].status, .connected)
        XCTAssertEqual(rows[0].detail, "Account login is active")
        XCTAssertNil(rows[0].endpointAlias)
        XCTAssertEqual(rows[0].id, "metabase-mcp")
        XCTAssertEqual(rows[1].status, .needsSignIn)
        XCTAssertEqual(rows[1].endpointAlias, "instamart")
        XCTAssertEqual(rows[1].id, "swiggy-mcp:instamart")
    }

    func testOlderEngineRowsReadAsUnknown() {
        let rows = BrowserServiceAccount.parse([["name": "higgsfield", "label": "Higgsfield account"], ["label": "no name"]])
        XCTAssertEqual(rows.count, 1)
        XCTAssertEqual(rows[0].status, .unknown)
        XCTAssertEqual(rows[0].status.title, "Unknown")
        XCTAssertNil(rows[0].detail)
        XCTAssertNil(rows[0].kind)
        XCTAssertNil(rows[0].managedBot)
        XCTAssertEqual(rows[0].methods, [])
    }

    func testDecodesManagedBotRowsAndUnavailable() {
        let rows = BrowserServiceAccount.parse([
            ["name": "google-workspace", "label": "Google Workspace (work)", "kind": "managed_bot", "bot": "gmail-work",
             "profile": "work", "methods": ["login_url"], "status": "connected"],
            ["name": "google-workspace", "label": "Google Workspace (business)", "kind": "managed_bot", "bot": "gmail-business",
             "profile": "business", "methods": ["login_url"], "status": "needs_sign_in"],
            ["name": "whatsapp", "label": "WhatsApp", "kind": "managed_bot", "bot": "whatsapp",
             "methods": ["pairing_code", "qr", "fax"], "status": "authenticating"],
            ["name": "telegram", "label": "Telegram", "kind": "managed_bot", "bot": "telegram-self",
             "methods": ["code", "qr"], "status": "unavailable", "detail": "Set TGCLI_API_ID and TGCLI_API_HASH"],
            ["name": "metabase-mcp", "label": "Provider account", "kind": "governed_oauth", "status": "connected"],
        ])
        XCTAssertEqual(rows.count, 5)
        XCTAssertEqual(rows[0].managedBot, "gmail-work")
        XCTAssertEqual(rows[0].profile, "work")
        XCTAssertEqual(rows[0].methods, [.loginURL])
        XCTAssertNotEqual(rows[0].id, rows[1].id, "Two Google profiles of one skill are separate rows")
        XCTAssertEqual(rows[2].methods, [.pairingCode, .qr])
        XCTAssertTrue(rows[2].methods[0].needsPhone)
        XCTAssertEqual(rows[2].methods[0].label(bot: "whatsapp"), "Link with phone number")
        XCTAssertEqual(rows[3].status, .unavailable)
        XCTAssertEqual(rows[3].status.title, "Unavailable")
        XCTAssertEqual(rows[3].detail, "Set TGCLI_API_ID and TGCLI_API_HASH")
        XCTAssertEqual(rows[3].methods.map { $0.label(bot: "telegram-self") }, ["Code", "QR"])
        XCTAssertEqual(rows[4].kind, .governedOAuth)
        XCTAssertNil(rows[4].managedBot)
    }

    func testManagedBotMethodsFallBackByBotName() {
        let rows = BrowserServiceAccount.parse([
            ["name": "telegram", "label": "Telegram", "kind": "managed_bot", "bot": "telegram-self"],
            ["name": "google-workspace", "label": "Google", "kind": "managed_bot", "bot": "gmail-work", "methods": []],
        ])
        XCTAssertEqual(rows[0].methods, [.code, .qr])
        XCTAssertEqual(rows[1].methods, [.loginURL])
    }

    func testDecodesBotConnectState() {
        let code = ManagedBotConnectState.parse(bot: "telegram-self", [
            "bot": "telegram-self", "status": "authenticating", "detail": "Enter the code Telegram sent you",
            "pending": ["kind": "code", "label": "Telegram code", "pairing_code": NSNull(), "qr_data_url": NSNull(), "sign_in": NSNull()],
        ])
        XCTAssertEqual(code.status, .authenticating)
        XCTAssertEqual(code.detail, "Enter the code Telegram sent you")
        XCTAssertEqual(code.pending?.kind, .code)
        XCTAssertEqual(code.pending?.label, "Telegram code")
        XCTAssertNil(code.pending?.pairingCode)

        let pairing = ManagedBotConnectState.parse(bot: "whatsapp", [
            "status": "authenticating", "pending": ["kind": "pairing_code", "pairing_code": "ABCD1234"],
        ])
        XCTAssertEqual(pairing.bot, "whatsapp")
        XCTAssertEqual(pairing.pending?.kind, .pairingCode)
        XCTAssertEqual(pairing.pending?.pairingCode, "ABCD1234")

        let qr = ManagedBotConnectState.parse(bot: "whatsapp", [
            "status": "authenticating", "pending": ["kind": "qr", "qr_data_url": "data:image/png;base64,iVBORw0KGgo="],
        ])
        XCTAssertEqual(qr.pending?.qrPNG, Data(base64Encoded: "iVBORw0KGgo="))
        let svg = ManagedBotConnectState.parse(bot: "whatsapp", [
            "status": "authenticating", "pending": ["kind": "qr", "qr_data_url": "data:image/svg+xml;base64,PHN2Zy8+"],
        ])
        XCTAssertNil(svg.pending?.qrPNG)

        let ticket = "bsi_" + String(repeating: "c", count: 64)
        let google = ManagedBotConnectState.parse(bot: "gmail-work", [
            "status": "authenticating",
            "pending": ["kind": "login_url", "sign_in": ["mode": "computer", "ticket": ticket,
                                                       "viewer_path": "/api/magician/v2/auth/browser-signin/view", "expires_in": 300]],
        ])
        XCTAssertEqual(google.pending?.kind, .loginURL)
        XCTAssertEqual(google.pending?.signInMode, "computer")
        XCTAssertEqual(google.pending?.signInTicket, ticket)

        let idle = ManagedBotConnectState.parse(bot: "gmail-work", ["status": "from_the_future", "pending": ["kind": "nope"]])
        XCTAssertEqual(idle.status, .unknown)
        XCTAssertNil(idle.pending)
    }
}
