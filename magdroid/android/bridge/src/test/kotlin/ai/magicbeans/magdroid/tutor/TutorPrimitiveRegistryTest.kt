package ai.magicbeans.magdroid.tutor

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Which primitives exist, and where a new one can come from.
 *
 * The registry is the only thing that knows the set, and it holds no hardcoded
 * type list at all — that is what makes a primitive a data decision.
 */
class TutorPrimitiveRegistryTest {

    private fun recipe(type: String, vararg aliases: String) = TutorRecipe(
        type = type,
        aliases = aliases.toList(),
        draw = listOf(
            RecipeOp(
                "line",
                mapOf(
                    "from" to RecipeValue.Arr(listOf(RecipeValue.Str("sx"), RecipeValue.Str("sy"))),
                    "to" to RecipeValue.Arr(listOf(RecipeValue.Str("ex"), RecipeValue.Str("ey"))),
                ),
            ),
        ),
    )

    @Test
    fun `a recipe answers to its type and every alias`() {
        val registry = TutorPrimitiveRegistry(listOf(recipe("arrow", "vector_arrow", "force_arrow")))
        assertEquals("arrow", registry.recipe("arrow")?.type)
        assertEquals("arrow", registry.recipe("vector_arrow")?.type)
        assertEquals("arrow", registry.recipe("force_arrow")?.type)
        assertTrue(registry.isSupported("FORCE_ARROW"))
        assertTrue(registry.isSupported("  arrow  "))
    }

    @Test
    fun `an unknown type is not supported`() {
        val registry = TutorPrimitiveRegistry(listOf(recipe("arrow")))
        assertNull(registry.recipe("hyperbola"))
        assertFalse(registry.isSupported("hyperbola"))
    }

    /**
     * The backend reads its global folder first and the scope's second so a
     * scope can redefine a built-in. The index has to resolve the collision the
     * same way, or a scope's override would be the one that loses.
     */
    @Test
    fun `a later recipe overrides an earlier one`() {
        val global = TutorRecipe(type = "rect", draw = listOf(RecipeOp("rect", emptyMap())))
        val scope = TutorRecipe(type = "rect", draw = listOf(RecipeOp("circle", emptyMap())))
        val registry = TutorPrimitiveRegistry(listOf(global, scope))
        assertEquals("circle", registry.recipe("rect")?.draw?.single()?.op)
    }

    @Test
    fun `replacing the set drops what came before`() {
        val registry = TutorPrimitiveRegistry(listOf(recipe("arrow")))
        registry.replaceAll(listOf(recipe("circle")))
        assertFalse(registry.isSupported("arrow"))
        assertTrue(registry.isSupported("circle"))
    }

    @Test
    fun `an empty registry supports nothing`() {
        assertTrue(TutorPrimitiveRegistry().isEmpty)
        assertNull(TutorPrimitiveRegistry().recipe("rect"))
    }

    /**
     * The whole point, proven end to end: a primitive this build has never
     * heard of arrives at runtime and draws. No branch here names `star`, and
     * none needs to.
     */
    @Test
    fun `a primitive invented at runtime draws without a rebuild`() {
        val invented = TutorRecipe.decodeList(
            """
            [{
              "type": "star",
              "draw": [
                { "op": "polygon", "points": [["cx","cy-r"], ["cx+r","cy+r"], ["cx-r","cy+r"]] }
              ]
            }]
            """.trimIndent(),
        )!!
        val registry = TutorPrimitiveRegistry(invented)
        assertTrue(registry.isSupported("star"))

        val shape = TutorShape(type = "star", cx = 100.0, cy = 100.0, r = 20.0)
        val geometry = RecipeInterpreter.geometry(registry.recipe("star")!!, shape)

        assertEquals(listOf("polygon"), geometry.map { it.op })
        assertTrue(geometry.single().closed)
        assertEquals(
            listOf(
                TutorPoint(100.0, 80.0),
                TutorPoint(120.0, 120.0),
                TutorPoint(80.0, 120.0),
            ),
            geometry.single().points,
        )
    }

    /** A malformed entry costs itself, not the rest of the set. */
    @Test
    fun `a bad recipe does not take the good ones with it`() {
        val decoded = TutorRecipe.decodeList(
            """[{"type":"good","draw":[]}, {"no_type":true}, {"type":"also_good","draw":[]}]""",
        )!!
        assertEquals(listOf("good", "also_good"), decoded.map { it.type })
    }

    /** Both envelopes the endpoint has served decode. */
    @Test
    fun `a bare array and a primitives wrapper both decode`() {
        val bare = TutorRecipe.decodeList("""[{"type":"rect","draw":[]}]""")
        val wrapped = TutorRecipe.decodeList("""{"primitives":[{"type":"rect","draw":[]}]}""")
        assertEquals(1, bare?.size)
        assertEquals(bare, wrapped)
        assertNull(TutorRecipe.decodeList("not json"))
    }
}
