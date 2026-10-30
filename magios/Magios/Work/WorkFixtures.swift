#if DEBUG
import Foundation

/// Deterministic native UI data. This transport is reachable only in a DEBUG
/// build launched with --work-ui-test-fixture; it never contacts a runtime.
final class WorkFixtureProtocol: URLProtocol {
    override class func canInit(with request: URLRequest) -> Bool { request.url?.host == "work-fixture.invalid" }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func stopLoading() {}
    override func startLoading() {
        let path = request.url?.path ?? ""
        let body: String
        if path.hasSuffix("/notes/tree") {
            body = ##"{"provider":"fixture-notes"}"##
        } else if path.hasSuffix("/notes/search") {
            body = ##"{"hits":[{"title":"Launch evidence","relative_path":"launch.md","source_ref":"notes:launch","provider":"fixture-notes"}]}"##
        } else if path.hasSuffix("/notes/file") {
            body = ##"{"title":"Launch evidence","relative_path":"launch.md","provider":"fixture-notes","markdown":"Exact provider source\nsecond line"}"##
        } else if path.hasSuffix("/memory/goal-memory") {
            body = ##"{"tier":{"scope":"goal","description":"Goal evidence"},"goal_id":"goal/+launch","data":"Exact goal memory","shared":false}"##
        } else if path.hasSuffix("/memory") {
            body = ##"{"tiers":[{"tier_name":"goal-memory","scope":"goal","has_data":true,"available_goal_ids":["goal/+launch"]},{"tier_name":"user-memory","scope":"user","has_data":true,"shared":false}]}"##
        } else if path.hasSuffix("/harness-overview") {
            body = ##"{"context":{"program":{"relative_path":"fixture.md"}},"evaluation":{"active_program_state":{"program":{"relative_path":"fixture.md"},"current_phase":"Verify migration","current_step":"Keep task controls","update_history":[{"reason":"Checked exact task evidence","task_id":"fixture-detail-done","execution_id":"exec-fixture"}]}}}"##
        } else if path.hasSuffix("/relentless/preview") {
            body = ##"{"contract_hash":"fixture-contract","budget_can_admit_attempt":true}"##
        } else if path.hasSuffix("/ui-threads") {
            body = ##"{"threads":[{"id":"general","name":"General"},{"id":"launch","name":"Launch"},{"id":"archived","name":"Archived","archived":true}]}"##
        } else if path.hasSuffix("/agents/health") {
            body = ##"{"agents":[{"agent_id":"fixture-crew","overall":{"score":0.92,"inputs":{"state":"healthy"}},"rolling_7d":{"calls":42,"spend_usd":3.18,"success_rate":0.95}},{"agent_id":"research-worker","overall":{"score":0.71,"inputs":{"state":"healthy"}},"rolling_7d":{"calls":17,"spend_usd":1.04,"success_rate":0.88}},{"agent_id":"ops-helper","overall":{"score":0.38,"inputs":{"state":"needs_attention"}},"rolling_7d":{"calls":9,"spend_usd":0.61,"success_rate":0.44}}]}"##
        } else if path.hasSuffix("/agents") {
            body = ##"{"agents":[{"definition":{"agent_id":"fixture-crew","name":"Fixture crew","kind":"Personal","persona":"Native fixture assistant","trust_level":"local","harness":{},"autonomous_config":{"schedule":"0 */4 * * *","max_tasks_per_cycle":3,"max_steps_per_plan":10,"focus_areas":[{"name":"Launch","description":"Prepare launch","program":"fixture.md","priority":"medium"}]},"description":"Native Work qualification","is_primary":true,"delegation_targets":["research-worker","missing-member"],"coordination":{"max_delegation_depth":2}},"etag":"v1","status":"active","current_cycle_id":"fixture-cycle","current_execution_id":"exec-fixture"},{"definition":{"agent_id":"research-worker","name":"Research worker","kind":"Worker","description":"Collects sources and drafts briefs for the launch team.","delegation_targets":[]},"etag":"v1","status":"paused"},{"definition":{"agent_id":"ops-helper","name":"Ops helper","kind":"Personal","description":"Watches the weekly operations review and flags blockers."},"etag":"v1","status":"error"},{"definition":{"agent_id":"archive-bot","name":"Archive bot","kind":"Worker","description":"Retired summariser kept for its history.","disabled":true},"etag":"v1","status":"idle"}],"system_agents":[{"agent_id":"planner","name":"Planner","description":"Breaks a goal into a reviewed plan."},{"agent_id":"critic","name":"Critic","description":"Scores drafts against the quality rubric."}]}"##
        } else if path.hasSuffix("/agents/fixture-crew") {
            body = ##"{"definition":{"agent_id":"fixture-crew","name":"Fixture crew","kind":"Personal","persona":"Native fixture assistant","trust_level":"local","harness":{},"autonomous_config":{"schedule":"0 */4 * * *","max_tasks_per_cycle":3,"max_steps_per_plan":10,"focus_areas":[{"name":"Launch","description":"Prepare launch","program":"fixture.md","priority":"medium"}]},"description":"Native Work qualification","user_memory_isolation":"fully_isolated","future_extension":{"budget":18446744073709551615},"model_pins":{"planning":"fixture-model"},"is_primary":true},"etag":"v1","status":"active","current_cycle_id":"fixture-cycle","current_execution_id":"exec-fixture"}"##
        } else if path.hasSuffix("/programs") {
            body = ##"{"programs":[{"name":"fixture.md","title":"Fixture program"},{"name":"launch-plan.md","title":"Launch plan"},{"name":"ops/weekly-review.md","title":"Weekly operations review"}]}"##
        } else if path.hasSuffix("/programs/fixture.md") {
            body = ##"{"name":"fixture.md","title":"Fixture program","content":"# Direction\nKeep the full task controls.","history":[{"snapshot":"fixture-v1","modified":"2026-10-06"}],"sha256":"fixture-sha256"}"##
        } else if path.hasSuffix("/campaigns/preview") {
            body = ##"{"spec":{"name":"Reviewed capture fixture","maximum_acts":1,"approval":{"always_review":true}}}"##
        } else if path.hasSuffix("/campaigns") {
            body = ##"{"campaigns":[{"status":"active","state":{"campaign":{"id":"fixture-campaign","version_hash":"campaign-v1","revoked_at":null,"spec":{"name":"Launch partner follow-ups","expires_at":"2026-11-01T00:00:00Z","maximum_acts":5,"targets":[{"task_id":"fixture-active","identity_id":"partner-a"}]}}}}],"identities":[{"id":"partner-a","label":"Partner A"}],"pagination":{"total":1,"has_more":false}}"##
        } else if path.hasSuffix("/captures") {
            body = ##"{"captures":[{"id":"capture-1","status":"awaiting_review","binding_hash":"binding-1","binding":{"run_id":"fixture-run","artifact_hash":"fixture-hash","identity_id":"partner-a"}}],"pagination":{"total":1,"has_more":false}}"##
        } else if path.hasSuffix("/guidance-proposals") {
            body = ##"{"proposals":[{"guidance":{"title":"Prefer cited sources"},"candidate_id":"candidate-1","agent_id":"fixture-crew","training_run_ids":["fixture-run"]}],"pagination":{"total":1,"has_more":false}}"##
        } else if path.hasSuffix("/relentless/work") {
            body = ##"{"targets":[{"task_id":"fixture-active","title":"Fixture active task","agent_id":"fixture-crew","policy":{"max_runs":3,"runs_started":1},"runs":[{"id":"fixture-run"}]}],"pagination":{"total":1,"has_more":false}}"##
        } else if path.hasSuffix("/proposals/fixture-proposal") {
            body = ##"{"origin":{"request_id":"fixture-proposal","agent_id":"fixture-crew"},"proposal":{"goal":"Tighten the launch checklist","budget":{"max_attempts":3}},"reason":"Two launch reviews missed the same step."}"##
        } else if path.hasSuffix("/v2/proposals") {
            body = ##"{"proposals":[{"id":"structural-1","source":"harness:fixture-crew","status":"pending","title":"Add a weekly review focus area","summary":"Split launch follow-ups into their own focus area."}]}"##
        } else if path.hasSuffix("/user-requests") {
            body = ##"{"requests":[{"id":"briefing-1","request_type":"harness.notify_owner.briefing","status":"pending","title":"Weekly briefing ready","context":{"owner_agent_id":"fixture-crew"},"options":[{"id":"acknowledge","label":"Acknowledge"}]}],"history":[]}"##
        } else if path.hasSuffix("/harness/runtime") {
            body = ##"{"enabled":true,"interval_secs":3600,"last_tick_at":"2026-10-18T08:00:00Z","next_tick_at":"2026-10-18T09:00:00Z"}"##
        } else if path.hasSuffix("/harness/anomalies") {
            body = ##"{"anomalies":[{"kind":"stalled_cycle","agent_id":"fixture-crew","detected_at":"2026-10-18T07:40:00Z","repair":"Restarted the cycle"}]}"##
        } else if path.hasSuffix("/guidance-policies") {
            body = "{\"policies\":[" + Self.learningPolicy + "],\"pagination\":{\"total\":1,\"has_more\":false}}"
        } else if path.hasSuffix("/guidance-policies/fixture-learning") {
            body = Self.learningPolicy
        } else if path.hasSuffix("/evidence/fixture-run") {
            body = ##"{"sealed":true,"content_hash":"changed-monitor-hash","evidence":{"marker":"untrusted-fixture-marker"}}"##
        } else if path.hasSuffix("/catalog") {
            body = ##"{"items":[{"kind":"run","item":{"id":"fixture-run","goal":"Fixture quality work","status":"accepted","label":"Review draft","attempt_count":1,"max_attempts":2}},{"kind":"proposal","item":{"goal":"Tighten the launch checklist","origin":{"request_id":"fixture-proposal","agent_id":"fixture-crew"}}},{"kind":"run","item":{"id":"fixture-run-2","goal":"Weekly digest polish","status":"running","label":"Drafting","attempt_count":2,"max_attempts":5}}],"pagination":{"total":3,"has_more":false}}"##
        } else if path.hasSuffix("/runs/fixture-run") {
            body = ##"{"id":"fixture-run","status":"accepted","contract":{"goal":"Fixture quality work","budget":{"max_attempts":2}},"admission":{"task_id":"fixture-active"},"attempts":[{"number":1,"artifact":{"hash":"previous-hash"}},{"number":2,"artifact":{"hash":"fixture-hash"}}],"routing":{"artifact_hash":"fixture-hash","route":"human_review"},"owner_approved":null,"handoff":null}"##
        } else if path.hasSuffix("/artifacts/fixture-hash") {
            body = ##"{"files":{"draft.md":"Exact fixture output"},"binary_files":{}}"##
        } else if path.hasSuffix("/artifacts/previous-hash") {
            body = ##"{"files":{"draft.md":"Previous fixture output"},"binary_files":{}}"##
        } else if path.hasSuffix("/profiles") {
            body = ##"{"profiles":[{"name":"actor","family":"A"},{"name":"critic","family":"B"}],"actor_default":"actor","critic_default":"critic"}"##
        } else { body = "{}" }
        let status = request.httpMethod == "PATCH" && path.hasSuffix("/agents/fixture-crew") && request.value(forHTTPHeaderField: "If-Match") != "v1" ? 412 : 200
        let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil, headerFields: ["Content-Type":"application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data(body.utf8)); client?.urlProtocolDidFinishLoading(self)
    }
    private static let learningPolicy = ##"{"policy":{"id":"fixture-learning","version_hash":"fixture-learning-version","spec":{"name":"Fixture learning policy","targets":[]}},"adoptions":{"slot":{"run_id":"fixture-run"}},"monitored":{"fixture-run":{"evidence_hash":"recorded-monitor-hash"}}}"##
    static func client() -> WorkAPIClient {
        let configuration = URLSessionConfiguration.ephemeral; configuration.protocolClasses = [WorkFixtureProtocol.self]
        return WorkAPIClient(baseURL: URL(string: "https://work-fixture.invalid")!, headers: [:], session: URLSession(configuration: configuration), currentCheck: { true })
    }
}
#endif
