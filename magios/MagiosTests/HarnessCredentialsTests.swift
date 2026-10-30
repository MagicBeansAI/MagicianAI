import XCTest
@testable import Magician

/// The engine pickers' credentials label (docs/components/magician/harness-auth.md).
/// The Harness credentials page itself lives in the embedded web Settings.
final class HarnessCredentialsTests: XCTestCase {
    private func json(_ object: Any) throws -> Data { try JSONSerialization.data(withJSONObject: object) }

    func testCredentialsLabelMapping() {
        XCTAssertEqual(HarnessCredentialsLabel.label("login"), "your login")
        XCTAssertEqual(HarnessCredentialsLabel.label("magician_key"), "Magician’s key")
        XCTAssertEqual(HarnessCredentialsLabel.label("none"), "not signed in")
        XCTAssertNil(HarnessCredentialsLabel.label("other"))
        XCTAssertNil(HarnessCredentialsLabel.label(nil))
        XCTAssertEqual(HarnessCredentialsLabel.engineName("Codex", engine: "codex", credentials: "login"), "Codex · your login")
        XCTAssertEqual(HarnessCredentialsLabel.engineName("Magician", engine: "magician", credentials: "login"), "Magician")
        XCTAssertEqual(HarnessCredentialsLabel.engineName("Grok", engine: "grok", credentials: nil), "Grok")
    }

    func testEngineRostersDecodeCredentialsLeniently() throws {
        let roster = try json(["engines": [
            ["name": "magician", "installed": true],
            ["name": "codex", "installed": true, "models": ["default"], "credentials": "magician_key"],
            ["name": "grok", "installed": true, "credentials": 7],
        ]])
        struct Roster: Decodable { let engines: [ChatHarnessOption] }
        let engines = try JSONDecoder().decode(Roster.self, from: roster).engines
        XCTAssertEqual(engines.map(\.credentials), [nil, "magician_key", nil])

        let catalog = try json([
            "models": [["id": "native/codex/m", "engine": "codex_app_server", "provider": "p", "connection": "p",
                        "model": "m", "label": "M", "choices": ["auto": "codex_app_server/auto"],
                        "thinking": ["mode": "effort", "levels": ["low"], "default": "low"]]],
            "engines": [["id": "codex_app_server", "selectable": true, "credentials": "login"],
                        ["id": "agy_cli", "selectable": false, "credentials": "none"]],
        ])
        let rows = try JSONDecoder().decode(VibeDevModelCatalog.self, from: catalog).rows
        XCTAssertEqual(rows.first { $0.engine == "codex_app_server" }?.credentials, "login")
        XCTAssertEqual(rows.first { $0.engine == "agy_cli" }?.credentials, "none")
    }

    func testSlashCommandEngineRowsCarryTheLabel() {
        let context = ComposerCommandContext(
            engines: [(name: "magician", models: ["default"]), (name: "codex", models: ["default", "gpt-5"])],
            engineCredentials: ["magician": "login", "codex": "login"])
        XCTAssertEqual(ComposerCommands.engineOptionLabel("codex", context), "Codex · your login")
        XCTAssertEqual(ComposerCommands.engineOptionLabel("magician", context), "Magician")
    }
}
