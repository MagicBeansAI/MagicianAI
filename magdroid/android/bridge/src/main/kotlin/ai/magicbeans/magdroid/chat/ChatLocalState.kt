package ai.magicbeans.magdroid.chat

import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/** Length prefixes keep account, server and session boundaries unambiguous. */
object ChatLocalState {
    /** Do not overwrite a field the owner did not edit from their snapshot. */
    fun threadSettingsPatch(threadId: String, originalName: String, originalMemory: String, name: String, memory: String): JsonObject = buildJsonObject {
        val title = name.trim().ifBlank { threadId }
        if (title != originalName) put("name", title)
        if (memory != originalMemory) put("memory_text", memory)
    }

    fun key(origin: String, principal: String, workspace: String, session: String?): String =
        "magdroid.chat.v2|" + listOf(origin, principal, workspace, session ?: "pending")
            .joinToString("|") { "${it.toByteArray(Charsets.UTF_8).size}:$it" }

    fun outputPath(raw: String): String? {
        val text = raw.trim()
        val uri = runCatching { java.net.URI(text.replace(" ", "%20")) }.getOrNull() ?: return null
        if (uri.scheme != null || uri.rawAuthority != null || uri.rawQuery != null || uri.rawFragment != null) return null
        val path = uri.path ?: return null
        if (path.startsWith("/api/") || listOf("/work", "/notes", "/tasks", "/chat", "/t/", "/settings", "/attention", "/vibedev").any { path.startsWith(it) } || path.startsWith("//")) return null
        if (path.split('/').any { it == ".." } || path.any { it.code < 32 }) return null
        return path.takeIf { it.contains('/') || it.substringAfterLast('.').let { ext -> ext != it && ext.isNotBlank() } }
    }

    fun usageSql(session: String, byModel: Boolean = false): String {
        val id = "'${session.replace("'", "''")}'"
        return "SELECT ${if (byModel) "provider, model, " else ""}SUM(measured_calls) AS calls, " +
            "CASE WHEN SUM(token_calls) > 0 THEN SUM(input_tokens) END AS input_tokens, " +
            "CASE WHEN SUM(token_calls) > 0 THEN SUM(output_tokens) END AS output_tokens, " +
            "CASE WHEN SUM(cache_read_calls) > 0 THEN SUM(cache_read_tokens) END AS cache_read_tokens, " +
            "CASE WHEN SUM(cache_write_calls) > 0 THEN SUM(cache_creation_tokens) END AS cache_creation_tokens, " +
            "CASE WHEN SUM(priced_calls) > 0 THEN SUM(cost_usd) END AS cost_usd, " +
            "SUM(measured_calls) - SUM(priced_calls) AS unpriced_calls FROM llm_cost_daily WHERE chat_session_id = $id" +
            if (byModel) " GROUP BY provider, model ORDER BY cost_usd DESC NULLS LAST, calls DESC" else ""
    }
}
