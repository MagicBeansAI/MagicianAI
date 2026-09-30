package ai.magicbeans.magdroid.chat

import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The composer's `@`-mention pipeline, against `MentionCatalog.swift` and the
 * web's `composerMentions.ts`.
 *
 * All three clients must insert the same token, because the backend parses it
 * out of the message text. A client that spells a mention differently is a
 * client whose mentions quietly do nothing.
 */
class MentionCatalogTest {
    private val json = Json { ignoreUnknownKeys = true }

    // MARK: markers

    /**
     * The marker is what the backend's tokenizer matches, not the item id.
     */
    @Test
    fun `feature lanes serialize to their marker`() {
        val items = MentionCatalog.featureItems().associateBy { it.id }
        assertEquals("@vibedev", items.getValue("feature:vibedev").serialized())
        assertEquals("@vibedev #discuss", items.getValue("feature:vibedev_discuss").serialized())
    }

    /**
     * The space between `@vibedev` and `#discuss` is load-bearing: `#` is a
     * token character in the backend tokenizer, so a glued `@vibedev#discuss`
     * is one unknown token and matches no marker at all.
     */
    @Test
    fun `the discuss flag is a separate token`() {
        val discuss = MentionCatalog.featureItems().first { it.id == "feature:vibedev_discuss" }
        assertTrue(discuss.serialized().contains(" #discuss"))
        assertTrue(!discuss.serialized().contains("@vibedev#"))
    }

    /**
     * A lane is offered when this client can honour it — either by executing it
     * natively or because the server dispatches it from the marker alone.
     *
     * This assertion previously required a lane to be *server-recognised*, and
     * excluded `@brainstorm` on that basis. Wrong rule: brainstorm is a client
     * lane on both phones, intercepted before the message is ever sent, so
     * "the server does not know it" was never the test. Never offering a lane
     * that needs a surface this client lacks still is.
     */
    @Test
    fun `every offered lane is one this client can honour`() {
        val ids = MentionCatalog.featureItems().map { it.id }
        assertEquals(
            listOf(
                "feature:copilot",
                "feature:brainstorm",
                "feature:tutor",
                "feature:tutor_quick",
                "feature:vibedev",
                "feature:vibedev_discuss",
            ),
            ids,
        )
        // Intercepted locally, so it never rides along as literal text.
        assertTrue(BrainstormInvoke.isInvoke("@brainstorm an idea"))
    }

    /**
     * App Copilot has no iOS counterpart, deliberately.
     *
     * iOS can neither screenshot another app nor tap in one, so this lane is
     * not a parity gap on that side — it is a capability the platform does not
     * have.
     */
    @Test
    fun `copilot serializes to its marker`() {
        val copilot = MentionCatalog.featureItems().first { it.id == "feature:copilot" }
        assertEquals("@copilot", copilot.serialized())
    }

    /** Tutor's quick flag is a separate token, for the tokenizer's sake. */
    @Test
    fun `tutor lanes serialize to their markers`() {
        val items = MentionCatalog.featureItems().associateBy { it.id }
        assertEquals("@tutor", items.getValue("feature:tutor").serialized())
        assertEquals("@tutor #quick", items.getValue("feature:tutor_quick").serialized())
    }

    /** Non-feature mentions serialize to their routing form unchanged. */
    @Test
    fun `other kinds serialize to their routing form`() {
        val built = MentionCatalog.build(
            agents = listOf(ReferenceAgent(agentId = "researcher", name = "Researcher")),
            skills = listOf(
                ReferenceSkill(name = "web_search", kind = "compiled"),
                ReferenceSkill(name = "terse", kind = "personality-mode"),
                ReferenceSkill(
                    name = "deploy", kind = "procedure", route = "delegate",
                    ownerAgentId = "ops", ownerAgentName = "Ops",
                ),
            ),
            tasks = listOf("task-7" to "Summarise the quarter"),
        ).associateBy { it.id }

        assertEquals("agent:researcher", built.getValue("agent:researcher").serialized())
        assertEquals("skill:web_search", built.getValue("tool:web_search").serialized())
        assertEquals("personality:terse", built.getValue("personality:terse").serialized())
        // A delegated skill names the agent it routes through; dropping that
        // would send it to whoever happens to be listening.
        assertEquals("skill:deploy via agent:ops", built.getValue("tool:deploy:via:ops").serialized())
        assertEquals("task:task-7", built.getValue("task:task-7").serialized())
    }

    // MARK: grouping

    @Test
    fun `singular and plural both narrow to a lane`() {
        assertEquals(MentionGroup.Kind(MentionKind.Agent), MentionCatalog.groupForQuery("agent"))
        assertEquals(MentionGroup.Kind(MentionKind.Agent), MentionCatalog.groupForQuery("agents"))
        assertEquals(MentionGroup.Kind(MentionKind.Tool), MentionCatalog.groupForQuery("skills:web"))
        assertEquals(MentionGroup.Kind(MentionKind.Task), MentionCatalog.groupForQuery("task:"))
        assertEquals(MentionGroup.Kind(MentionKind.Feature), MentionCatalog.groupForQuery("features"))
        assertEquals(MentionGroup.All, MentionCatalog.groupForQuery("vib"))
    }

    @Test
    fun `the needle is what follows the lane`() {
        val group = MentionCatalog.groupForQuery("agent:res")
        assertEquals("res", MentionCatalog.needleForQuery("agent:res", group))
        assertEquals("", MentionCatalog.needleForQuery("agent", group))
        assertEquals("vib", MentionCatalog.needleForQuery("vib", MentionGroup.All))
    }

    // MARK: filtering

    @Test
    fun `a bare at surfaces the feature lanes first`() {
        val items = MentionCatalog.build(agents = listOf(ReferenceAgent(agentId = "a")))
        assertEquals("feature:copilot", MentionCatalog.matchesFor(items, "").first().id)
    }

    @Test
    fun `search covers more than the label`() {
        val items = MentionCatalog.featureItems()
        // "ship" appears only in the search text, not the label or detail.
        assertEquals(1, MentionCatalog.matchesFor(items, "ship").size)
        assertEquals("feature:vibedev", MentionCatalog.matchesFor(items, "ship").first().id)
    }

    /** The list is capped so the picker never grows past the composer. */
    @Test
    fun `matches are capped`() {
        val many = (1..40).map { ReferenceAgent(agentId = "agent-$it") }
        assertEquals(MentionCatalog.LIMIT, MentionCatalog.matchesFor(MentionCatalog.build(agents = many), "").size)
    }

    // MARK: trigger detection

    @Test
    fun `a bare trigger is detected and consumes itself`() {
        val trigger = MentionCatalog.detectTrigger("hello @vib")!!
        assertEquals("vib", trigger.query)
        // `@vib` is four characters, all of which the pick replaces.
        assertEquals(4, trigger.consume)
    }

    @Test
    fun `a trigger at the very start is detected`() {
        val trigger = MentionCatalog.detectTrigger("@")!!
        assertEquals("", trigger.query)
        assertEquals(1, trigger.consume)
    }

    /**
     * Prose after a mention closes the picker. Leaving it open over the rest of
     * the sentence is what made the old detection unusable.
     */
    @Test
    fun `a finished word closes the picker`() {
        assertNull(MentionCatalog.detectTrigger("@vibedev now build it"))
        assertNull(MentionCatalog.detectTrigger("no mention here"))
    }

    /**
     * An email address is not a mention. Looking for the last `@` found one
     * here and opened the picker mid-address.
     */
    @Test
    fun `an email address does not trigger`() {
        assertNull(MentionCatalog.detectTrigger("write to ada@example.com"))
    }

    @Test
    fun `the command form carries its lane`() {
        val trigger = MentionCatalog.detectTrigger("@agent res")!!
        assertEquals("agent:res", trigger.query)
        // The whole `@agent res` span is replaced, not just the tail.
        assertEquals("@agent res".length, trigger.consume)
    }

    @Test
    fun `the command form works with an empty tail`() {
        val trigger = MentionCatalog.detectTrigger("@skill ")!!
        assertEquals("skill:", trigger.query)
    }

    // MARK: insertion

    /**
     * Only the trigger's span is replaced, so a mention picked mid-sentence
     * leaves the words on either side alone.
     */
    @Test
    fun `picking replaces only the trigger`() {
        val draft = "please @vib"
        val trigger = MentionCatalog.detectTrigger(draft)!!
        val item = MentionCatalog.featureItems().first { it.id == "feature:vibedev" }
        assertEquals("please @vibedev ", draft.dropLast(trigger.consume) + item.serialized() + " ")
    }

    // MARK: wire

    /**
     * The catalog is two lists, not a flat array. The client modelled it as
     * `references`, which the endpoint has never sent.
     */
    @Test
    fun `the reference catalog decodes agents and skills`() {
        val catalog = json.decodeFromString(
            ReferenceCatalogResponse.serializer(),
            """
            {"agents": [{"agent_id": "researcher", "name": "Researcher",
                         "description": "Finds things", "route": "delegate"}],
             "skills": [{"name": "web_search", "kind": "compiled", "route": "self",
                         "owner_agent_id": null, "owner_agent_name": null}]}
            """,
        )
        assertEquals("researcher", catalog.agents.single().agentId)
        assertEquals("web_search", catalog.skills.single().name)

        val built = MentionCatalog.build(catalog.agents, catalog.skills)
        assertTrue(built.any { it.id == "agent:researcher" })
        assertTrue(built.any { it.id == "tool:web_search" })
    }

    /** A catalog with neither list is empty, not a failure. */
    @Test
    fun `an empty catalog leaves the feature lanes`() {
        val catalog = json.decodeFromString(ReferenceCatalogResponse.serializer(), "{}")
        assertEquals(MentionCatalog.featureItems().size, MentionCatalog.build(catalog.agents, catalog.skills).size)
    }

    /** The same agent twice is one entry; the catalog can repeat itself. */
    @Test
    fun `duplicate agents collapse`() {
        val built = MentionCatalog.build(
            agents = listOf(ReferenceAgent(agentId = "dup"), ReferenceAgent(agentId = "dup")),
        )
        assertEquals(1, built.count { it.id == "agent:dup" })
    }
}
