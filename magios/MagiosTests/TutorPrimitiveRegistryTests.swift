import XCTest
@testable import Magician

final class TutorPrimitiveRegistryTests: XCTestCase {
    override func tearDown() {
        MockURLProtocol.handler = nil
        super.tearDown()
    }

    // A recipe with a NOVEL type not known to any native code — the proof that a
    // runtime-served recipe renders without an app rebuild.
    private let novelType = "sparkle_burst"
    private var novelRecipeJSON: [String: Any] {
        [
            "type": novelType,
            "aliases": ["twinkle"],
            "version": 1,
            "defaults": ["size": 20],
            "draw": [
                ["op": "circle", "cx": "cx", "cy": "cy", "r": "r|size"],
                ["op": "line", "from": ["cx-(r|size)", "cy"], "to": ["cx+(r|size)", "cy"]]
            ]
        ]
    }

    // MARK: - Injected set is exposed synchronously

    func testInjectedSetExposesRecipesAndAliases() throws {
        let recipe = try JSONDecoder().decode(
            TutorRecipe.self, from: jsonData(novelRecipeJSON))
        let registry = TutorPrimitiveRegistry(recipes: [recipe])

        XCTAssertTrue(registry.isSupported(novelType))
        XCTAssertTrue(registry.isSupported("twinkle"))              // alias
        XCTAssertTrue(registry.isSupported("SPARKLE_BURST"))        // case-insensitive
        XCTAssertNotNil(registry.recipe(for: novelType))
        XCTAssertFalse(registry.isSupported("does_not_exist"))
    }

    // MARK: - Network success → exposed

    func testRefreshAdoptsNetworkSet() async throws {
        // Registry starts empty; the backend serves the novel recipe → adopted.
        let registry = TutorPrimitiveRegistry(recipes: [])
        XCTAssertFalse(registry.isSupported(novelType))

        let payload = jsonData(["primitives": [novelRecipeJSON]])
        MockURLProtocol.handler = { request in
            XCTAssertTrue(request.url?.absoluteString.contains("/tutor/primitives") ?? false)
            return (response(for: request), payload)
        }

        let ok = await registry.refresh(session: makeMockSession())
        XCTAssertTrue(ok)
        XCTAssertTrue(registry.isSupported(novelType))
        XCTAssertTrue(registry.isSupported("twinkle"))
    }

    func testRefreshAcceptsBareArrayPayload() async throws {
        let registry = TutorPrimitiveRegistry(recipes: [])
        let payload = jsonData([novelRecipeJSON])   // bare array, no envelope
        MockURLProtocol.handler = { request in (response(for: request), payload) }

        let ok = await registry.refresh(session: makeMockSession())
        XCTAssertTrue(ok)
        XCTAssertTrue(registry.isSupported(novelType))
    }

    // MARK: - Network failure → keeps the current (bundled/injected) set

    func testRefreshFailureKeepsExistingSet() async throws {
        let recipe = try JSONDecoder().decode(TutorRecipe.self, from: jsonData(novelRecipeJSON))
        let registry = TutorPrimitiveRegistry(recipes: [recipe])

        MockURLProtocol.handler = { _ in throw URLError(.notConnectedToInternet) }
        let ok = await registry.refresh(session: makeMockSession())

        XCTAssertFalse(ok)
        // The previously-loaded set survives a failed refresh.
        XCTAssertTrue(registry.isSupported(novelType))
    }

    func testRefreshMalformedResponseKeepsExistingSet() async throws {
        let recipe = try JSONDecoder().decode(TutorRecipe.self, from: jsonData(novelRecipeJSON))
        let registry = TutorPrimitiveRegistry(recipes: [recipe])

        MockURLProtocol.handler = { request in
            (response(for: request), Data("not json".utf8))
        }
        let ok = await registry.refresh(session: makeMockSession())

        XCTAssertFalse(ok)
        XCTAssertTrue(registry.isSupported(novelType))
    }

    // MARK: - Bundled set (shared singleton)

    func testSharedRegistryLoadsBundledBuiltins() {
        // The bundled recipe set covers the built-in types — proves the offline
        // fallback loads synchronously from the app bundle.
        for type in ["rect", "highlight", "arrow", "circle", "label", "callout",
                     "line", "field_line", "path", "polygon", "area_fill",
                     "cursive_text", "curve", "right_angle_marker", "angle_marker",
                     "square_on_segment"] {
            XCTAssertTrue(TutorPrimitiveRegistry.shared.isSupported(type),
                          "\(type) should be a bundled built-in")
        }
        // Aliases resolve to their recipe too.
        XCTAssertTrue(TutorPrimitiveRegistry.shared.isSupported("force_arrow"))
        XCTAssertTrue(TutorPrimitiveRegistry.shared.isSupported("free_body_body"))
    }

    // MARK: - Runtime-injected NOVEL type renders (the no-rebuild property)

    func testNovelRuntimeRecipeRendersWithoutRebuild() throws {
        let recipe = try JSONDecoder().decode(TutorRecipe.self, from: jsonData(novelRecipeJSON))
        let registry = TutorPrimitiveRegistry(recipes: [recipe])

        // The novel type is unknown to any native switch, yet the registry finds
        // a recipe and the interpreter produces geometry for it — no app rebuild.
        XCTAssertTrue(registry.isSupported(novelType))
        let looked = try XCTUnwrap(registry.recipe(for: novelType))

        let shape = try JSONDecoder().decode(
            TutorShape.self, from: Data(#"{"type":"sparkle_burst","cx":40,"cy":40,"r":15}"#.utf8))
        let geo = RecipeInterpreter.geometry(for: looked, shape: shape)

        // circle + line ops both resolve → two geometry entries.
        XCTAssertEqual(geo.map(\.op), ["circle", "line"])
        // line spans cx-r … cx+r at cy: (25,40)→(55,40).
        XCTAssertEqual(geo[1].points, [CGPoint(x: 25, y: 40), CGPoint(x: 55, y: 40)])
    }
}
