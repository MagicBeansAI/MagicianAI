package ai.magicbeans.magdroid.chat

import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

private val profileJson = Json { ignoreUnknownKeys = true }

class ChatProfileMetadataTest {
    @Test
    fun `typed model catalog supplies defaults and admissions without profile rows`() {
        val catalog = profileJson.decodeFromString<ChatModelCatalog>("""
            {"models":[{"id":"openai/gpt-6.1-sol","model":"gpt-6.1-sol","provider":"openai","connection":"openai","label":"Sol",
            "capabilities":{"vision":true},"thinking":{"mode":"effort","levels":["low","high"],"default":"low"},
            "choices":{"auto":"new-auto","high":"new-high"}}],"default_selection":{"model_id":"openai/gpt-6.1-sol","thinking":"auto"}}
        """)
        val rows = catalog.rows()
        assertEquals(listOf("new-auto", "new-high"), rows.map { it.name })
        assertEquals("new-auto", rows.single { it.isDefault }.name)
        val selection = rows.single { it.name == "new-high" }.modelSelection
        for (engine in listOf("magician", "pi")) {
            val fields = ChatModelPreference.requestFields(selection, engine, "stale-native-model")
            assertEquals(engine, fields["harness_engine"]?.toString()?.trim('"'))
            assertTrue(fields.containsKey("model_selection"))
            assertTrue(!fields.containsKey("profile") && !fields.containsKey("harness_model"))
        }
        val native = ChatModelPreference.requestFields(selection, "claude_code", "claude-opus-5")
        assertTrue(native.containsKey("harness_model") && !native.containsKey("model_selection"))
        val unavailable = ChatModelPreference.resolve(catalog.copy(defaultSelection = null).rows(), "", null)
        assertEquals("Choose a model to continue.", unavailable.issue)
    }

    @Test
    fun `returning offline clients migrate frozen selections after alias removal`() {
        val choice = ChatModelChoice(
            id = "openai/gpt-6.1-sol", choices = mapOf("auto" to "new-auto", "high" to "new-high"),
        )
        val rows = listOf(
            ChatProfile(name = "new-auto", modelChoice = choice),
            ChatProfile(name = "new-high"),
        )
        val old = "chat-gpt61sol-responses-vision-toolsauto-thinking"
        val migrated = ChatModelPreference.resolve(rows, old, null)
        assertEquals("new-high", migrated.route)
        assertEquals(ChatModelSelection(choice.id, "high"), migrated.selection)
        assertNull(migrated.issue)
        val typed = ChatModelPreference.resolve(rows, old, ChatModelSelection(choice.id, "auto"))
        assertEquals("new-auto", typed.route)
        assertEquals(ChatModelPreference.unavailableRoute, ChatModelPreference.resolve(rows.take(1), old, null).route)
        assertNull(LegacyLlmSelections.chatSelection("op-embedding-local"))
        assertEquals("none", LegacyLlmSelections.chatSelection("chat-dsv4-flash-messages-toolsauto-rnone")?.thinking)
        assertEquals("auto", LegacyLlmSelections.chatSelection("chat-anthropic-adaptive-frontier")?.thinking)
    }

    @Test
    fun `model catalog keeps native efforts and opaque route handles`() {
        val profile = profileJson.decodeFromString<ChatProfile>(
            """{"name":"saved-profile","model_selection":{"model_id":"openai/sol","thinking":"high"},
                "model_choice":{"id":"openai/sol","provider":"openai","connection":"openai",
                "model":"gpt-6.1-sol","label":"GPT Sol","limits":null,
                "thinking":{"mode":"effort","levels":["low","medium","high"],"default":"medium"},
                "operation_choices":{"auto":"opaque-initial"},
                "choices":{"auto":"opaque-auto","high":"opaque-high"}}}""",
        )
        assertEquals("openai/sol", profile.modelSelection?.modelId)
        assertEquals("high", profile.modelSelection?.thinking)
        assertEquals("opaque-high", profile.modelChoice?.choices?.get("high"))
        assertNull(profile.modelChoice?.choices?.get("none"))
        assertNull(profile.modelChoice?.limits)
        assertEquals("opaque-initial", profile.modelChoice?.operationChoices?.get("auto"))
        assertEquals("Extra high", thinkingLevelLabel("xhigh"))
        val legacy = profileJson.decodeFromString<ChatProfile>("""{"name":"old"}""")
        assertNull(legacy.modelSelection)
        assertNull(legacy.modelChoice)
        val coding = profileJson.decodeFromString<ai.magicbeans.magdroid.vibedev.VibeDevCodingProfile>(
            """{"id":"saved-coding","engine":"pi","model_selection":{"model_id":"openai/sol","thinking":"high"},
                "model_choice":{"id":"openai/sol","provider":"openai","connection":"openai","model":"gpt-6.1-sol","label":"GPT Sol",
                "thinking":{"mode":"effort","levels":["low","medium","high"],"default":"medium"},
                "choices":{"auto":"opaque-coding-auto","high":"opaque-coding-high"}}}""",
        )
        assertEquals("high", coding.modelSelection?.thinking)
        assertEquals("opaque-coding-high", coding.modelChoice?.choices?.get("high"))

        val auto = profile.copy(name = "opaque-auto", modelSelection = profile.modelSelection?.copy(thinking = "auto"))
        val high = profile.copy(name = "opaque-high", modelChoice = null)
        val migrated = ChatModelPreference.resolve(listOf(profile, auto, high), profile.name, null)
        assertEquals("opaque-high", migrated.route)
        assertEquals(profile.modelSelection, migrated.selection)
        assertNull(migrated.issue)
        val restored = ChatModelPreference.resolve(listOf(auto, high), "", migrated.selection)
        assertEquals("opaque-high", restored.route)
        assertNull(restored.issue)
        val unavailable = ChatModelPreference.resolve(listOf(auto), auto.name, migrated.selection)
        assertEquals(ChatModelPreference.unavailableRoute, unavailable.route)
        assertEquals(migrated.selection, unavailable.selection)
        assertEquals(ChatModelPreference.unavailableMessage, unavailable.issue)
        val unknown = ChatModelPreference.resolve(listOf(auto), "retired-unknown", null)
        assertEquals("retired-unknown", unknown.route)
        assertEquals(ChatModelPreference.unavailableMessage, unknown.issue)
    }

    @Test
    fun `thinking labels match the web catalog mapping`() {
        val expected = mapOf(
            "auto" to "Auto", "none" to "Off", "on" to "On", "minimal" to "Minimal", "low" to "Low",
            "medium" to "Medium", "high" to "High", "xhigh" to "Extra high", "max" to "Max", "ultra" to "Ultra",
        )
        expected.forEach { (level, label) -> assertEquals(label, thinkingLevelLabel(level)) }
        assertEquals("turbo", thinkingLevelLabel("turbo"))
    }

    @Test
    fun `composer chip shows thinking only for catalog engines and thinking models`() {
        val effort = ChatModelChoice(id = "openai/sol", label = "GPT Sol",
            thinking = ChatThinkingControl(mode = "effort", levels = listOf("low", "high")))
        val flat = effort.copy(id = "local/flat", thinking = ChatThinkingControl(mode = "none"))
        val high = ChatModelSelection(effort.id, "high")
        assertEquals("High", chatComposerThinkingLabel("magician", effort, high))
        assertEquals("High", chatComposerThinkingLabel("pi", effort, high))
        assertEquals("Auto", chatComposerThinkingLabel("magician", effort, null))
        assertNull(chatComposerThinkingLabel("claude_code", effort, high))
        assertNull(chatComposerThinkingLabel("magician", flat, ChatModelSelection(flat.id, "none")))
        assertNull(chatComposerThinkingLabel("magician", null, high))
        // VibeDev's coding chip has no engine gate, only the model rule.
        assertEquals("Extra high", modelChipThinkingLabel(effort, high.copy(thinking = "xhigh")))
        assertNull(modelChipThinkingLabel(flat, null))
        assertEquals("Chat engine: magician, selection: Sol, thinking: High",
            modelChipAccessibilityLabel("Chat engine: magician, selection: Sol", "High"))
        assertEquals("Chat engine: codex, selection: default",
            modelChipAccessibilityLabel("Chat engine: codex, selection: default", null))
    }
}
