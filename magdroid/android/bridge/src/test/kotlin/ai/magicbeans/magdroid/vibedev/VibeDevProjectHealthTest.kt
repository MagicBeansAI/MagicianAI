package ai.magicbeans.magdroid.vibedev

import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** A project's `health`: decoded leniently, absent is ok, and the web titles per level. */
class VibeDevProjectHealthTest {
    private fun project(health: String?): VibeDevProject = vibeDevJson.decodeFromString(
        VibeDevProject.serializer(),
        """{"project_id":"p1","name":"Shop"${health?.let { ",\"health\":$it" }.orEmpty()}}""",
    )

    @Test fun absent_or_ok_health_needs_no_attention() {
        for (health in listOf(null, """{"status":"ok"}""", "null", "\"broken\"", "[]")) {
            val decoded = project(health).health
            assertEquals(health, VibeDevHealthLevel.Ok, decoded.level)
            assertFalse(health, decoded.needsAttention)
            assertNull(health, decoded.title)
        }
    }

    @Test fun error_health_reads_runs_cannot_start_with_each_message() {
        val health = project("""{"status":"error","issues":[
            {"code":"folder_missing","severity":"error","message":"The folder ~/code/shop is gone."},
            {"code":"chat_session_missing","severity":"warning","message":"The project chat was closed."}
        ]}""").health
        assertEquals(VibeDevHealthLevel.Error, health.level)
        assertTrue(health.needsAttention)
        assertEquals("Project needs attention: runs cannot start.", health.title)
        assertEquals(listOf("The folder ~/code/shop is gone.", "The project chat was closed."), health.issues.map { it.message })
        assertEquals(listOf(VibeDevHealthLevel.Error, VibeDevHealthLevel.Warning), health.issues.map { it.severity })
        assertEquals("folder_missing", health.issues.first().code)
    }

    @Test fun warning_health_reads_needs_attention() {
        val health = project("""{"status":"warning","issues":[{"code":"chat_session_missing","severity":"warning","message":"Chat closed."}]}""").health
        assertEquals(VibeDevHealthLevel.Warning, health.level)
        assertEquals("Project needs attention.", health.title)
    }

    @Test fun unknown_codes_statuses_and_malformed_issues_are_tolerated() {
        val health = project("""{"status":"degraded","issues":[
            {"code":"brand_new_code","severity":"error","message":"Something new."},
            {"code":"x","severity":"error"},
            {"code":"y","severity":"loud","message":"Odd severity."},
            7
        ]}""").health
        assertEquals(VibeDevHealthLevel.Error, health.level)
        assertEquals(listOf("Something new.", "Odd severity."), health.issues.map { it.message })
        assertEquals(VibeDevHealthLevel.Warning, health.issues[1].severity)
        assertEquals(VibeDevHealthLevel.Warning, project("""{"issues":[{"severity":"warning","message":"m"}]}""").health.level)
    }

    @Test fun issues_carry_their_title_and_a_known_action() {
        val health = project("""{"status":"warning","issues":[
            {"code":"not_a_git_repository","severity":"warning","title":"Folder isn't a git repository",
             "message":"Runs use a sandboxed copy.","action":{"kind":"git_init","label":"Make this a git repository"}},
            {"code":"later","severity":"warning","title":"  ","message":"m2","action":{"kind":"reindex","label":"Reindex"}},
            {"code":"bad","severity":"warning","message":"m3","action":{"kind":"git_init"}},
            {"code":"worse","severity":"warning","message":"m4","action":"git_init","title":5}
        ]}""").health
        assertEquals(VibeDevHealthLevel.Warning, health.level)
        assertEquals("Project needs attention.", health.title)
        val (git, unknown, noLabel, wrongShape) = health.issues
        assertEquals("Folder isn't a git repository", git.title)
        assertEquals(VibeDevHealthAction("git_init", "Make this a git repository"), git.action)
        assertTrue(git.action!!.isSupported)
        assertNull("a blank title is no title", unknown.title)
        assertFalse("an unknown kind gets no button", unknown.action!!.isSupported)
        assertNull(noLabel.action)
        assertNull(wrongShape.action)
        assertNull(wrongShape.title)
    }

    @Test fun an_unhealthy_start_shows_the_server_message() {
        val body = vibeDevJson.parseToJsonElement("""{"error":"vibedev_project_unhealthy",
            "message":"Project “Shop” cannot start a run: The folder ~/code/shop is gone.",
            "health":{"status":"error","issues":[{"code":"folder_missing","severity":"error","message":"The folder ~/code/shop is gone."}]}}""").jsonObject
        assertEquals("Project “Shop” cannot start a run: The folder ~/code/shop is gone.",
            VibeDevRepository.failureMessage(409, body as JsonObject))
    }
}
