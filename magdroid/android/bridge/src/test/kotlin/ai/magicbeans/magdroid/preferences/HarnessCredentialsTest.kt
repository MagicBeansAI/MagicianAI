package ai.magicbeans.magdroid.preferences

import ai.magicbeans.magdroid.chat.ChatHarnessRoster
import ai.magicbeans.magdroid.chat.ComposerCommandContext
import ai.magicbeans.magdroid.chat.ComposerCommandEngine
import ai.magicbeans.magdroid.chat.ComposerCommandId
import ai.magicbeans.magdroid.chat.ComposerCommands
import ai.magicbeans.magdroid.chat.chatJson
import ai.magicbeans.magdroid.vibedev.VibeDevModelCatalog
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Engine-picker credentials labels (web `harnessAuth.ts`). The harness
 * credentials setting itself is edited in the embedded web Settings.
 */
class HarnessCredentialsTest {
    @Test
    fun `credentials labels follow the web wording`() {
        assertEquals("your login", HarnessCredentials.label("login"))
        assertEquals("Magician’s key", HarnessCredentials.label("magician_key"))
        assertEquals("not signed in", HarnessCredentials.label("none"))
        assertNull(HarnessCredentials.label("borrowed"))
        assertNull(HarnessCredentials.label(null))
        assertEquals("Codex · your login", HarnessCredentials.withCredentials("Codex", "login"))
        assertEquals("Codex", HarnessCredentials.withCredentials("Codex", "something"))
        assertEquals("Magician", HarnessCredentials.engineName("magician", "Magician", "login"))
        assertEquals("Grok · not signed in", HarnessCredentials.engineName("grok", "Grok", "none"))
    }

    @Test
    fun `engine rows decode credentials leniently`() {
        val roster = chatJson.decodeFromString(
            ChatHarnessRoster.serializer(),
            """{"engines":[
                {"name":"magician","installed":true},
                {"name":"codex","installed":true,"credentials":"login"},
                {"name":"grok","installed":true,"credentials":{"odd":true}},
                {"name":"pi","installed":true,"credentials":"magician_key"}]}""",
        )
        assertEquals(listOf(null, "login", null, "magician_key"), roster.engines.map { it.credentials })

        val catalog = chatJson.decodeFromString(
            VibeDevModelCatalog.serializer(),
            """{"models":[{"id":"m","engine":"codex","label":"M","choices":{"auto":"codex/auto"}}],
                "engines":[{"id":"codex","selectable":true,"credentials":"none"},{"id":"agy_cli","selectable":false,"credentials":7}]}""",
        )
        val rows = catalog.rows()
        assertEquals("none", rows.single { it.engine == "codex" }.credentials)
        assertNull(rows.single { it.engine == "agy_cli" }.credentials)
    }

    @Test
    fun `composer engine pickers append the credentials label`() {
        val context = ComposerCommandContext(
            engines = listOf(
                ComposerCommandEngine("magician", listOf("default")),
                ComposerCommandEngine("codex", listOf("default", "gpt-5"), credentials = "login"),
                ComposerCommandEngine("grok", listOf("default"), credentials = "none"),
            ),
        )
        val state = ComposerCommands.open(ComposerCommandId.RunEngine).state
        // Back, the "Now" note and "Same as Chat" come first; models are a second level.
        val labels = ComposerCommands.rows("/run-engine ", state, context).drop(3).map { it.label }
        assertEquals(listOf("Magician", "Codex · your login", "Grok · not signed in"), labels)
        // Confirmations name the engine only.
        assertTrue(ComposerCommands.chatEngineConfirmation("codex", "default", false).endsWith(": Codex"))
    }
}
