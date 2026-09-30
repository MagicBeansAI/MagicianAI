import Foundation

private func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data("app contract fixture failure: \(message)\n".utf8))
    exit(1)
}

guard AppContractFixturesGenerated.contractVersion == "1.0.0" else {
    fail("unexpected contract version")
}
guard AppContractFixturesGenerated.protocolVersion == "1" else {
    fail("unexpected protocol version")
}

let digestPattern = try NSRegularExpression(pattern: "^blake3:[0-9a-f]{64}$")
for digest in [
    AppContractFixturesGenerated.schemaDigest,
    AppContractFixturesGenerated.fixtureDigest,
] {
    let range = NSRange(digest.startIndex..<digest.endIndex, in: digest)
    guard digestPattern.firstMatch(in: digest, range: range) != nil else {
        fail("invalid generated digest \(digest)")
    }
}

let expectedNames: Set<String> = [
    "action_cancellation_receipt",
    "action_cancellation_request",
    "action_composition_request",
    "action_composition_waiting",
    "action_launch",
    "query_request",
    "query_page",
    "mutation_command",
    "action_invocation",
    "action_result_completed",
    "action_result_uncertain",
    "action_run_completed",
    "action_run_running",
    "artifact_projection",
    "contract_capabilities",
    "error_stale_revision",
]
guard Set(AppContractFixturesGenerated.jsonByName.keys) == expectedNames else {
    fail("fixture catalog drifted")
}

for (name, source) in AppContractFixturesGenerated.jsonByName {
    guard let sourceData = source.data(using: .utf8) else {
        fail("\(name) is not UTF-8")
    }
    let decoded = try JSONSerialization.jsonObject(with: sourceData)
    guard JSONSerialization.isValidJSONObject(decoded) else {
        fail("\(name) did not decode to a JSON object")
    }
    let canonical = try JSONSerialization.data(withJSONObject: decoded, options: [.sortedKeys])
    let roundTripped = try JSONSerialization.jsonObject(with: canonical)
    guard (decoded as AnyObject).isEqual(roundTripped) else {
        fail("\(name) changed during the Swift JSON round trip")
    }
}

guard
    let mutationSource = AppContractFixturesGenerated.jsonByName["mutation_command"],
    let mutationData = mutationSource.data(using: .utf8),
    let mutation = try JSONSerialization.jsonObject(with: mutationData) as? [String: Any],
    (mutation["idempotency_key"] as? String)?.hasPrefix("mutation-key:") == true,
    mutation["atomicity"] as? String == "all_or_nothing",
    let actionSource = AppContractFixturesGenerated.jsonByName["action_invocation"],
    let actionData = actionSource.data(using: .utf8),
    let action = try JSONSerialization.jsonObject(with: actionData) as? [String: Any],
    (action["idempotency_key"] as? String)?.hasPrefix("action-key:") == true,
    let artifactSource = AppContractFixturesGenerated.jsonByName["artifact_projection"],
    let artifactData = artifactSource.data(using: .utf8),
    let artifact = try JSONSerialization.jsonObject(with: artifactData) as? [String: Any],
    artifact["source"] as? String == "artifact_projection",
    let artifactValue = artifact["value"] as? [String: Any],
    artifactValue["media_type"] as? String == "video/mp4"
else {
    fail("idempotency, atomicity or artifact semantics drifted")
}

guard
    let uncertainSource = AppContractFixturesGenerated.jsonByName["action_result_uncertain"],
    let uncertainData = uncertainSource.data(using: .utf8),
    let uncertain = try JSONSerialization.jsonObject(with: uncertainData) as? [String: Any],
    uncertain["status"] as? String == "uncertain",
    let error = uncertain["error"] as? [String: Any],
    error["code"] as? String == "external_outcome_uncertain",
    error["disposition"] as? String == "outcome_uncertain"
else {
    fail("uncertain external-effect semantics drifted")
}

print("Swift app contract fixtures round-tripped successfully")
