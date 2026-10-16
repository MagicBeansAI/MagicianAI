package ai.magicbeans.magdroid.preferences

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** The owner's preferences on Android: the view, the history and edit outcomes. */
class OwnerPreferencesTest {

    @Test
    fun `the view reads every preference as checks see it`() {
        val view = parsePreferenceView(
            """
            {"intentions":[
              {"id":"p1","revision":"r1","kind":"standing_instruction","assertion_status":"direct_owner_statement",
               "statement":"Check with me before ordering meat.","conditions":null,"lifecycle":"active",
               "source":{"native_tier":{"tier":"preferences"}},"editable":true,"last_checked_at_ms":1700000000000},
              {"id":"p2","revision":"r2","kind":"taste","assertion_status":"inferred","statement":"Decaf after 3pm",
               "lifecycle":"unresolved","source":"taste_profile","editable":true},
              {"revision":"no id, skipped"}],
             "retired":[{"id":"p0","statement":"No dairy","lifecycle":"superseded","superseded_by":"p3"}],
             "omitted":[{"id":"x","reason":"unknown_lifecycle"}],
             "coverage":{"status":"partial"}}
            """,
        )
        assertEquals(listOf("p1", "p2"), view.preferences.map { it.id })
        val rule = view.preferences[0]
        assertEquals(PreferenceKind.StandingInstruction, rule.kind)
        assertEquals("preferences", rule.source)
        assertEquals("You said it", rule.statusLabel)
        assertFalse(rule.confirmable)
        val inferred = view.preferences[1]
        assertTrue(inferred.confirmable)
        assertTrue(inferred.unresolved)
        assertEquals("taste profile", inferred.source)
        assertEquals("p3", view.retired.single().supersededBy)
        assertEquals(1, view.omitted)
        assertEquals("partial", view.coverage)
    }

    @Test
    fun `history names who changed it and from where, and never shows erased wording`() {
        val entries = parsePreferenceHistory(
            """
            {"entries":[
              {"at_ms":2,"record_id":"p1","op":"edited","actor":{"via":"owner_surface","surface":"android"},
               "wording_before":"No meat","wording_after":"No red meat","wording_erased":false},
              {"at_ms":1,"record_id":"p9","op":"forgotten","actor":{"via":"owner_surface","surface":"ios"},
               "wording_before":null,"wording_after":null,"wording_erased":true}]}
            """,
        )
        assertEquals("Reworded", entries[0].opLabel)
        assertEquals("you on Android", entries[0].whoLabel)
        assertEquals("you on iPhone", entries[1].whoLabel)
        assertTrue(entries[1].wordingErased)
    }

    @Test
    fun `a stale edit saves nothing and names the current revision`() {
        val stale = parseEditOutcome(
            409,
            """{"error":"stale_preference","current_revision":"r9","message":"It changed since you opened it."}""",
        )
        assertEquals(PreferenceEditOutcome.Stale("r9", "It changed since you opened it."), stale)
        assertEquals(PreferenceEditOutcome.Applied("p1"), parseEditOutcome(201, """{"record_id":"p1"}"""))
    }

    @Test
    fun `a stored key and json object reads as a title and sorted fields`() {
        val presented = presentStatement(
            """output_format_preference: {"media_type":"text/markdown","scope":"both","depth":2}"""
        )
        assertEquals("Output format preference", presented?.title)
        assertEquals(
            listOf("Depth" to "2", "Media type" to "text/markdown", "Scope" to "both"),
            presented?.fields,
        )
        assertEquals("UI test fallback", presentStatement("""ui_test_fallback: {"api_url":"x"}""")?.title)
        assertEquals(null, presentStatement("Check with me before ordering meat."))
        assertEquals(null, presentStatement("Note: {not json}"))
        assertEquals(null, presentStatement("list: [1,2]"))
    }

    @Test(expected = PreferencesError::class)
    fun `a refused edit is an error with the server's words`() {
        parseEditOutcome(403, """{"error":"private_surface_required","message":"Not here."}""")
    }
}
