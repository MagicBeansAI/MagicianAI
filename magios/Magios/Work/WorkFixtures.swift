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
        } else if path.hasSuffix("/agents") {
            body = ##"{"agents":[{"definition":{"agent_id":"fixture-crew","name":"Fixture crew","kind":"Personal","persona":"Native fixture assistant","trust_level":"local","harness":{},"autonomous_config":{"schedule":"0 */4 * * *","max_tasks_per_cycle":3,"max_steps_per_plan":10,"focus_areas":[{"name":"Launch","description":"Prepare launch","program":"fixture.md","priority":"medium"}]},"description":"Native Work qualification","is_primary":true},"etag":"v1","status":"active","current_cycle_id":"fixture-cycle","current_execution_id":"exec-fixture"}]}"##
        } else if path.hasSuffix("/agents/fixture-crew") {
            body = ##"{"definition":{"agent_id":"fixture-crew","name":"Fixture crew","kind":"Personal","persona":"Native fixture assistant","trust_level":"local","harness":{},"autonomous_config":{"schedule":"0 */4 * * *","max_tasks_per_cycle":3,"max_steps_per_plan":10,"focus_areas":[{"name":"Launch","description":"Prepare launch","program":"fixture.md","priority":"medium"}]},"description":"Native Work qualification","user_memory_isolation":"fully_isolated","future_extension":{"budget":18446744073709551615},"model_pins":{"planning":"fixture-model"},"is_primary":true},"etag":"v1","status":"active","current_cycle_id":"fixture-cycle","current_execution_id":"exec-fixture"}"##
        } else if path.hasSuffix("/programs") {
            body = ##"{"programs":[{"name":"fixture.md","title":"Fixture program"}]}"##
        } else if path.hasSuffix("/programs/fixture.md") {
            body = ##"{"name":"fixture.md","title":"Fixture program","content":"# Direction\nKeep the full task controls.","history":[{"snapshot":"fixture-v1","modified":"2026-10-06"}]}"##
        } else if path.hasSuffix("/campaigns/preview") {
            body = ##"{"spec":{"name":"Reviewed capture fixture","maximum_acts":1,"approval":{"always_review":true}}}"##
        } else if path.hasSuffix("/campaigns") {
            body = ##"{"campaigns":[],"identities":[],"pagination":{"total":0,"has_more":false}}"##
        } else if path.hasSuffix("/guidance-policies") {
            body = "{\"policies\":[" + Self.learningPolicy + "],\"pagination\":{\"total\":1,\"has_more\":false}}"
        } else if path.hasSuffix("/guidance-policies/fixture-learning") {
            body = Self.learningPolicy
        } else if path.hasSuffix("/evidence/fixture-run") {
            body = ##"{"sealed":true,"content_hash":"changed-monitor-hash","evidence":{"marker":"untrusted-fixture-marker"}}"##
        } else if path.hasSuffix("/catalog") {
            body = ##"{"items":[{"kind":"run","item":{"id":"fixture-run","goal":"Fixture quality work","status":"accepted","label":"Review draft","attempt_count":1,"max_attempts":2}}],"pagination":{"total":1,"has_more":false}}"##
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
