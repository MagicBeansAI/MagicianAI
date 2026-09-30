import XCTest
@testable import Magician

final class MentionCatalogTests: XCTestCase {
    private let items = [
        ComposerMentionItem(id: "agent:researcher", label: "Researcher", kind: .agent,
                            insertText: "agent:researcher", searchText: "research web"),
        ComposerMentionItem(id: "tool:tavily", label: "Tavily", detail: "Search", kind: .tool,
                            insertText: "skill:tavily", searchText: "tavily search"),
        ComposerMentionItem(id: "personality:concise", label: "Concise", kind: .personality,
                            insertText: "personality:concise")
    ]

    func testQueryAliasesChooseExpectedGroups() {
        XCTAssertEqual(mentionGroupForQuery("agents:"), .kind(.agent))
        XCTAssertEqual(mentionGroupForQuery("skills:search"), .kind(.tool))
        XCTAssertEqual(mentionGroupForQuery("personality:"), .kind(.personality))
        XCTAssertEqual(mentionGroupForQuery("tasks:"), .kind(.task))
        XCTAssertEqual(mentionGroupForQuery("features:"), .kind(.feature))
        XCTAssertEqual(mentionGroupForQuery("anything"), .all)
    }

    func testNeedleDropsQualifiedPrefix() {
        XCTAssertEqual(mentionNeedleForQuery("agent:Res", group: .kind(.agent)), "res")
        XCTAssertEqual(mentionNeedleForQuery("Res", group: .all), "res")
    }

    func testMatchesFilterByGroupSearchTextAndLimit() {
        XCTAssertEqual(mentionMatchesFor(items, query: "agent:sea").map(\.id), ["agent:researcher"])
        XCTAssertEqual(mentionMatchesFor(items, query: "search").map(\.id), ["agent:researcher", "tool:tavily"])
        XCTAssertEqual(mentionMatchesFor(items, query: "", limit: 2).count, 2)
    }

    func testDetectsBareAndCommandTriggers() {
        XCTAssertEqual(detectMentionTrigger("please ask @rese"), MentionTrigger(consume: 5, query: "rese"))
        XCTAssertEqual(detectMentionTrigger("use @agent rese"), MentionTrigger(consume: 11, query: "agent:rese"))
        XCTAssertNil(detectMentionTrigger("email a@b.com"))
        XCTAssertNil(detectMentionTrigger("@agent researcher continue"))
    }

    func testFeatureSerializationMatchesBackendCommands() {
        let features = buildFeatureMentionItems()
        XCTAssertEqual(features.map(\.serialized),
                       ["@tutor", "@tutor #quick", "@brainstorm", "@vibedev", "@vibedev #discuss"])
        XCTAssertEqual(items[0].serialized, "agent:researcher")
    }

    func testVibedevFeatureSerializesDiscussAsASeparateToken() {
        let features = buildFeatureMentionItems()
        XCTAssertEqual(features.first { $0.id == "feature:vibedev" }?.serialized, "@vibedev")
        // The space is load-bearing — `#` is a token character in the backend
        // tokenizer, so a glued `@vibedev#discuss` would match no marker.
        XCTAssertEqual(features.first { $0.id == "feature:vibedev_discuss" }?.serialized,
                       "@vibedev #discuss")
        XCTAssertEqual(features.first { $0.id == "feature:vibedev_discuss" }?.insertText,
                       "feature:vibedev_discuss")
    }

    func testVibedevFeaturesAreAlwaysOfferedAndSearchWithoutCatchingOtherLanes() {
        // No gate to pass: unlike the web builder (`includeCopilot`), the iOS
        // builder takes no options, so both lanes are unconditional.
        let features = buildFeatureMentionItems()
        XCTAssertEqual(features.filter { $0.id.hasPrefix("feature:vibedev") }.map(\.id),
                       ["feature:vibedev", "feature:vibedev_discuss"])
        XCTAssertEqual(features.first { $0.id == "feature:vibedev" }?.chipLabel, "@vibedev")
        XCTAssertEqual(features.first { $0.id == "feature:vibedev_discuss" }?.chipLabel,
                       "@vibedev_discuss")
        // Precondition for the search assertions: no other lane advertises
        // "vibedev". The rail deliberately does NOT answer to "code" — the
        // feature is named VibeDev, and an alias would reintroduce the
        // ordinary-English overlap the rename removed.
        XCTAssertTrue(features
            .filter { !$0.id.hasPrefix("feature:vibedev") }
            .allSatisfy { !($0.searchText ?? "").contains("vibedev") })
        XCTAssertEqual(mentionMatchesFor(features, query: "vibedev").map(\.id),
                       ["feature:vibedev", "feature:vibedev_discuss"])
        XCTAssertEqual(mentionMatchesFor(features, query: "feature:discuss").map(\.id),
                       ["feature:vibedev_discuss"])
        XCTAssertTrue(mentionMatchesFor(features, query: "code").isEmpty)
    }

    func testBrainstormFeatureAcceptsCanonicalAndCommonMisspellingAndKeepsSeed() {
        XCTAssertTrue(BrainstormInvoke.isBrainstormInvoke("@brainstorm a calmer onboarding"))
        XCTAssertTrue(BrainstormInvoke.isBrainstormInvoke("@brainstrom rethink pricing"))
        XCTAssertTrue(BrainstormInvoke.isBrainstormInvoke("@brainstorm: rethink pricing"))
        XCTAssertFalse(BrainstormInvoke.isBrainstormInvoke("hey brainstrom, rethink pricing"))
        XCTAssertFalse(BrainstormInvoke.isBrainstormInvoke("we brainstormed yesterday"))
        XCTAssertFalse(BrainstormInvoke.isBrainstormInvoke("@brainstorm@example.com sent this"))
        XCTAssertFalse(BrainstormInvoke.isBrainstormInvoke("@brainstrom@example.com sent this"))
        XCTAssertEqual(BrainstormInvoke.strip("@brainstorm a calmer onboarding"), "a calmer onboarding")
        XCTAssertEqual(BrainstormInvoke.strip("@brainstrom rethink pricing"), "rethink pricing")
        XCTAssertEqual(BrainstormInvoke.strip("@brainstorm: rethink pricing"), "rethink pricing")
        XCTAssertEqual(BrainstormInvoke.strip("@brainstorm"), "")
    }

    func testTaskBuilderRejectsIncompleteTasksAndPreservesTitle() {
        let built = buildTaskMentionItems([
            (id: " task-1 ", title: " Ship app "),
            (id: nil, title: "No ID"),
            (id: "task-2", title: " ")
        ])
        XCTAssertEqual(built.count, 1)
        XCTAssertEqual(built[0].id, "task:task-1")
        XCTAssertEqual(built[0].insertText, "task:task-1")
        XCTAssertEqual(built[0].chipLabel, "Ship app")
    }

    func testCatalogBuilderDeduplicatesAgentsAndRoutesDelegatedSkills() throws {
        let agents = try JSONDecoder().decode([ComposerReferenceAgentEntry].self, from: jsonData([
            ["agent_id": "researcher", "name": "Researcher", "description": "Find facts", "route": "delegate"],
            ["agent_id": "researcher", "name": "Duplicate", "route": "delegate"],
            ["agent_id": "self", "name": "Main", "route": "self"]
        ]))
        let skills = try JSONDecoder().decode([ComposerSkillEntry].self, from: jsonData([
            ["name": "search", "description": "Search web", "kind": "procedure", "owner_agent_id": "researcher", "owner_agent_name": "Researcher", "route": "delegate"],
            ["name": "friendly", "kind": "personality-mode"]
        ]))

        let built = buildComposerMentionItems(agents: agents, skills: skills)
        XCTAssertEqual(built.filter { $0.kind == .agent }.count, 2)
        XCTAssertEqual(built.first { $0.id == "tool:search:via:researcher" }?.insertText,
                       "skill:search via agent:researcher")
        XCTAssertEqual(built.first { $0.kind == .personality }?.insertText, "personality:friendly")
        XCTAssertEqual(built.filter { $0.kind == .feature }.map(\.id),
                       ["feature:tutor", "feature:tutor_quick", "feature:brainstorm",
                        "feature:vibedev", "feature:vibedev_discuss"])
    }

    func testKindLabelsCoverEveryKind() {
        XCTAssertEqual(ComposerMentionKind.allForTests.map(mentionKindLabel),
                       ["Agent", "Tool", "Personality", "Task", "Feature"])
    }
}

private extension ComposerMentionKind {
    static let allForTests: [Self] = [.agent, .tool, .personality, .task, .feature]
}
