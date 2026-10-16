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
        assertNotNull(ready.crewSet(listOf("coordination", "max_delegation_depth"), JsonPrimitive(50)).crewValidation())
        assertNotNull(ready.crewSet(listOf("autonomous_config"), buildJsonObject {}).crewValidation())
    }
}
