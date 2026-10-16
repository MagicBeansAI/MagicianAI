package ai.magicbeans.magdroid.today

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class MuijModelsTest {
    @Test fun representative_published_dashboard_is_typed_and_readable() {
        val raw = todayJson.parseToJsonElement("""{
          "muij_version":"1.0","agent_id":"surface-briefing","generated_at":"2026-08-10T00:00:00Z",
          "layout":[{"id":"root","component_type":"Stack","label":"Briefing","props":{},"children":[
            {"id":"metrics","component_type":"Grid","label":"Metrics","props":{"columns":2},"children":[
              {"id":"revenue","component_type":"MetricCard","label":"Revenue","props":{"value":"${'$'}12.4k","trend":"up"}}
            ]},
            {"id":"records","component_type":"Table","label":"Campaigns","props":{
              "columns":[{"key":"name","label":"Campaign"}],"rows":[{"name":"Search"}]
            }}
          ]}]
        }""")

        val result = MuijDocument.parse(raw)
        assertTrue(result is MuijParseResult.Valid)
        val document = (result as MuijParseResult.Valid).document
        assertEquals("${'$'}12.4k", document.layout.single().children.first().children.single().metricValue)
        assertEquals("Campaign", document.layout.single().children.last().tableColumns.single().label)
        assertEquals("Search", document.layout.single().children.last().tableRows.single()["name"].compactDisplay())
    }

    @Test fun duplicate_ids_and_excessive_depth_fail_closed() {
        fun component(id: String, children: List<kotlinx.serialization.json.JsonElement> = emptyList()) = JsonObject(mapOf(
            "id" to JsonPrimitive(id), "component_type" to JsonPrimitive("Stack"),
            "label" to JsonPrimitive(id), "props" to JsonObject(emptyMap()), "children" to JsonArray(children),
        ))
        val duplicate = JsonObject(mapOf(
            "muij_version" to JsonPrimitive("1.0"), "agent_id" to JsonPrimitive("surface"),
            "layout" to JsonArray(listOf(component("same"), component("same"))),
        ))
        assertEquals(MuijParseResult.Invalid(MuijInvalidReason.DuplicateId("same")), MuijDocument.parse(duplicate))

        var nested: kotlinx.serialization.json.JsonElement = component("leaf")
        repeat(MuijDocument.MaximumDepth) { depth -> nested = component("node-$depth", listOf(nested)) }
        val deep = JsonObject(mapOf(
            "muij_version" to JsonPrimitive("1.0"), "agent_id" to JsonPrimitive("surface"),
            "layout" to JsonArray(listOf(nested)),
        ))
        assertEquals(MuijParseResult.Invalid(MuijInvalidReason.NestingTooDeep), MuijDocument.parse(deep))
    }

    @Test fun unknown_display_components_remain_forward_compatible() {
        val raw = todayJson.parseToJsonElement("""{
          "muij_version":"1.0","agent_id":"surface","layout":[
            {"id":"future","component_type":"FutureDisplay","label":"A future component","props":{}}
          ]
        }""")
        val document = (MuijDocument.parse(raw) as MuijParseResult.Valid).document
        assertEquals("FutureDisplay", document.layout.single().type)
        assertEquals("A future component", document.layout.single().displayLabel)
    }

    @Test fun arbitrary_json_fallback_is_human_readable_and_bounded() {
        val raw = todayJson.parseToJsonElement("""{"run_count":4,"healthy":true,"records":[1,2]}""")
        val rows = raw.presentationRows(limit = 2)
        assertEquals(2, rows.size)
        assertTrue(rows.any { it.key == "Healthy" && it.value == "Yes" })
    }

    @Test fun malformed_component_shape_fails_closed() {
        val raw = todayJson.parseToJsonElement("""{
          "muij_version":"1.0","agent_id":"surface","layout":[
            {"id":"broken","component_type":"Stack","label":"Broken","props":{},"children":{"not":"an array"}}
          ]
        }""")
        assertEquals(
            MuijParseResult.Invalid(MuijInvalidReason.InvalidComponent("broken")),
            MuijDocument.parse(raw),
        )
    }
}
