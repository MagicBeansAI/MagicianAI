package ai.magicbeans.magdroid.chat

import org.junit.Assert.*
import org.junit.Test

class ChatLocalStateTest {
    @Test fun `thread edits preserve untouched fields and permit explicitly clearing memory`() {
        val renamed = ChatLocalState.threadSettingsPatch("general", "Old name", "Saved context", "New name", "Saved context")
        assertEquals("\"New name\"", renamed["name"].toString())
        assertFalse(renamed.containsKey("memory_text"))
        val cleared = ChatLocalState.threadSettingsPatch("general", "Old name", "Saved context", "Old name", "")
        assertFalse(cleared.containsKey("name"))
        assertEquals("\"\"", cleared["memory_text"].toString())
        assertTrue(ChatLocalState.threadSettingsPatch("general", "Name", "Context", " Name ", "Context").isEmpty())
    }

    @Test fun `chat settings and drafts have independent machine account workspace and session keys`() {
        val first = ChatLocalState.key("https://one", "owner", "home", "chat-1")
        assertNotEquals(first, ChatLocalState.key("https://two", "owner", "home", "chat-1"))
        assertNotEquals(first, ChatLocalState.key("https://one", "guest", "home", "chat-1"))
        assertNotEquals(first, ChatLocalState.key("https://one", "owner", "work", "chat-1"))
        assertNotEquals(first, ChatLocalState.key("https://one", "owner", "home", "chat-2"))
        assertNotEquals(first, ChatLocalState.key("https://one", "owner", "home", null))
        assertNotEquals(ChatLocalState.key("a|b", "c", "d", "e"), ChatLocalState.key("a", "b|c", "d", "e"))
    }
    @Test fun `only file references go to session output resolver`() {
        assertEquals("Pages/Chapter-Map.md", ChatLocalState.outputPath("Pages/Chapter-Map.md"))
        assertEquals("Chapter Map.md", ChatLocalState.outputPath("Chapter%20Map.md"))
        listOf("https://other.test/file.md", "//other.test/file.md", "javascript:evil", "../private.md", "%2e%2e/private.md", "/work?task=x", "/notes?path=x").forEach {
            assertNull(it, ChatLocalState.outputPath(it))
        }
    }
    @Test fun `zero review budgets are submitted as unlimited without changing finite budgets`() {
        val unlimited = ai.magicbeans.magdroid.work.WorkQualityDraft(attempts = 0, minutes = 0, dollars = 0).proposal()
        val budget = unlimited["budget"] as kotlinx.serialization.json.JsonObject
        for (field in listOf("max_attempts", "max_wall_clock_secs", "max_cost_microusd")) {
            assertEquals("0", budget[field].toString())
        }
        val finite = ai.magicbeans.magdroid.work.WorkQualityDraft(dollars = 50).proposal()
        assertEquals("50000000", (finite["budget"] as kotlinx.serialization.json.JsonObject)["max_cost_microusd"].toString())
    }
    @Test fun `whole chat accounting uses persisted rollup and preserves unknown values`() {
        val sql = ChatLocalState.usageSql("chat'1")
        assertTrue(sql.contains("chat_session_id = 'chat''1'"))
        assertTrue(sql.contains("FROM llm_cost_daily"))
        assertTrue(sql.contains("CASE WHEN SUM(priced_calls) > 0"))
        assertTrue(sql.contains("unpriced_calls"))
        assertTrue(sql.contains("cache_creation_tokens"))
        val models = ChatLocalState.usageSql("chat'1", byModel = true)
        assertTrue(models.contains("chat_session_id = 'chat''1'"))
        assertTrue(models.contains("GROUP BY provider, model"))
    }
}
