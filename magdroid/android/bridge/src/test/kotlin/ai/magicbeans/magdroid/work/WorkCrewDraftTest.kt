package ai.magicbeans.magdroid.work

import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class WorkCrewDraftTest {
    @Test fun hierarchyReportsLongCyclesAndStopsAtMissingTargets() {
        fun member(id: String, vararg targets: String) = WorkCrewMember(id, buildJsonObject { putJsonArray("delegation_targets") { targets.forEach { add(it) } } }, "", "")
        val members = listOf(member("a", "b", "missing"), member("b", "c"), member("c", "a"), member("d", "*"))
        assertEquals(listOf("a", "b", "c", "a"), crewDelegationCycle("a", "b", members))
        assertNull(crewDelegationCycle("a", "missing", members)); assertNull(crewDelegationCycle("d", "*", members))
    }
    @Test fun guidedEditsRetainNestedExtensionsAndU64Budgets() {
        val original = Json.parseToJsonElement("""{"agent_id":"writer","name":"Writer","persona":"Help","trust_level":"local","kind":"Personal","budget":18446744073709551615,"harness":{"program_section":"Old","provider":{"mode":"keep"}},"autonomous_config":{"schedule":"0 9 * * *","future":true,"focus_areas":[{"name":"Launch","description":"Verify","priority":"high","evidence":{"revision":18446744073709551615}}]}}""") as JsonObject
        val changed = original.crewSet(listOf("harness", "program_section"), JsonPrimitive("Engineering"))
        assertEquals("18446744073709551615", (changed["budget"] as JsonPrimitive).content)
        assertEquals(original.crewAt("harness", "provider"), changed.crewAt("harness", "provider"))
        assertEquals(original["autonomous_config"], changed["autonomous_config"])
        assertNull(changed.crewValidation("writer")); assertNotNull(changed.crewValidation("other"))
        assertEquals(changed, Json.parseToJsonElement(changed.toString()))
        val removed = changed.crewSet(listOf("harness", "provider"), null).crewMergePatch(original)
        assertEquals(JsonNull, removed.crewAt("harness", "provider"))
        assertEquals(JsonPrimitive("Engineering"), removed.crewAt("harness", "program_section"))
        assertFalse(removed.containsKey("budget"))
    }
    @Test fun creationAndInvalidCyclesAreValidatedBeforePosting() {
        assertNotNull(newCrewDefinition().crewValidation())
        val ready = newCrewDefinition().crewSet(listOf("name"), JsonPrimitive("Writer"))
        assertNull(ready.crewValidation())
        assertNotNull(ready.crewSet(listOf("agent_id"), JsonPrimitive("Bad ID")).crewValidation())
        assertNotNull(ready.crewSet(listOf("constraints", "coordination", "max_delegation_depth"), JsonPrimitive(50)).crewValidation())
        assertNotNull(ready.crewSet(listOf("autonomous_config"), buildJsonObject {}).crewValidation())
        // The server's definition has no top-level `coordination` and stores
        // `kind` lowercase; drafts must match it to save.
        assertFalse(newCrewDefinition().containsKey("coordination"))
        assertEquals("worker", newCrewDefinition().text("kind"))
    }
    @Test fun crewKindIsReadCaseInsensitively() {
        val stored = Json.parseToJsonElement("""{"agent_id":"writer","name":"Writer","persona":"Help","trust_level":"local","kind":"personal"}""") as JsonObject
        assertTrue(stored.isPersonalCrew())
        assertNull(stored.crewValidation())
        assertTrue((Json.parseToJsonElement("""{"kind":"Personal"}""") as JsonObject).isPersonalCrew())
        assertFalse((Json.parseToJsonElement("""{"kind":"worker"}""") as JsonObject).isPersonalCrew())
    }
    @Test fun focusSharedDocumentsRoundTripTheirThreeStatesWithoutDisturbingOtherFields() {
        val area = Json.parseToJsonElement("""{"name":"Inbound","description":"Sweep","program":"outward_charter.md","scope":["*"],"future":{"n":18446744073709551615}}""") as JsonObject
        assertEquals(FocusSharedDocs.Global, area.focusSharedDocs())
        assertEquals(FocusSharedDocs.Global, area.crewSet(listOf("shared_programs"), JsonNull).focusSharedDocs())

        val none = area.withFocusSharedDocs(FocusSharedDocs.None)
        assertEquals(JsonArray(emptyList()), none["shared_programs"])
        assertEquals(FocusSharedDocs.None, none.focusSharedDocs())

        val chosen = none.toggleFocusSharedDoc("facts.md", true).toggleFocusSharedDoc("positioning.md", true)
        assertEquals(FocusSharedDocs.Chosen, chosen.focusSharedDocs())
        assertEquals("""["facts.md","positioning.md"]""", chosen["shared_programs"].toString())
        assertEquals(listOf("positioning.md"), chosen.toggleFocusSharedDoc("facts.md", false).focusSharedNames())
        // Unchecking the last one leaves an explicit "none", never the global list.
        assertEquals(JsonArray(emptyList()), chosen.toggleFocusSharedDoc("facts.md", false).toggleFocusSharedDoc("positioning.md", false)["shared_programs"])

        val global = chosen.withFocusSharedDocs(FocusSharedDocs.Global)
        assertFalse(global.containsKey("shared_programs"))
        assertEquals(area, global)
        // A stored name missing from the program list survives until it is removed.
        val stored = Json.parseToJsonElement("""{"name":"A","description":"B","shared_programs":["gone.md"," facts.md ","facts.md",3]}""") as JsonObject
        assertEquals(listOf("gone.md", "facts.md"), stored.focusSharedNames())
        assertEquals(listOf("gone.md", "facts.md"), stored.withFocusSharedDocs(FocusSharedDocs.Chosen).focusSharedNames())

        // Through the definition: the merge patch sends the whole array, so absent and [] stay distinct on the wire.
        fun definition(focus: JsonObject) = newCrewDefinition().crewSet(listOf("autonomous_config"), buildJsonObject { put("schedule", "0 9 * * *"); put("focus_areas", JsonArray(listOf(focus))) })
        val original = definition(chosen)
        val patch = definition(global).crewMergePatch(original)
        assertFalse((patch.crewAt("autonomous_config", "focus_areas") as JsonArray).single().jsonObject.containsKey("shared_programs"))
        assertEquals(JsonArray(emptyList()), (definition(none).crewMergePatch(original).crewAt("autonomous_config", "focus_areas") as JsonArray).single().jsonObject["shared_programs"])
        assertEquals("18446744073709551615", ((global["future"] as JsonObject)["n"] as JsonPrimitive).content)
    }
}
