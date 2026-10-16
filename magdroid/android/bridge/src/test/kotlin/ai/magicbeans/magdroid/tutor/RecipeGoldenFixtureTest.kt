package ai.magicbeans.magdroid.tutor

import java.io.File
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.doubleOrNull
import kotlinx.serialization.json.booleanOrNull
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The cross-platform parity guarantee for the recipe interpreter.
 *
 * These are the SAME files the web vitest suite and the Swift
 * `RecipeInterpreterTests` load — `docs/components/magician/tutor-primitive-fixtures`.
 * Each is a self-contained `{recipe, shape, space, expected}` case whose
 * `expected` is the space-coordinate geometry every interpreter must produce.
 *
 * Asserting against shared fixtures rather than against numbers written here is
 * the entire point: a test that encodes what this implementation happens to do
 * passes while the client draws something the other two do not. If the three
 * interpreters drift, one of the three suites goes red.
 */
class RecipeGoldenFixtureTest {

    private val json = Json { ignoreUnknownKeys = true; isLenient = true }

    /** Floating point, across three languages — compare with a tolerance. */
    private val tolerance = 1e-6

    @Test
    fun `every shared fixture produces the geometry all three clients agree on`() {
        val files = fixtureDirectory().listFiles { file -> file.extension == "json" }
            ?.sortedBy { it.name }
            .orEmpty()
        assertTrue("no fixtures found — the shared directory moved?", files.isNotEmpty())

        files.forEach { file ->
            val root = json.parseToJsonElement(file.readText()) as JsonObject
            val label = (root["name"] as? JsonPrimitive)?.content ?: file.name

            val recipe = TutorRecipe.from(root.getValue("recipe"))
                ?: error("$label: the fixture's recipe did not decode")
            val shape = json.decodeFromString(
                TutorShape.serializer(),
                root.getValue("shape").toString(),
            )

            val expected = (root.getValue("expected") as JsonArray).map { entry ->
                val obj = entry as JsonObject
                OpGeometry(
                    op = (obj.getValue("op") as JsonPrimitive).content,
                    points = (obj.getValue("points") as JsonArray).map { pair ->
                        val xy = pair as JsonArray
                        TutorPoint(
                            (xy[0] as JsonPrimitive).doubleOrNull ?: 0.0,
                            (xy[1] as JsonPrimitive).doubleOrNull ?: 0.0,
                        )
                    },
                    closed = (obj["closed"] as? JsonPrimitive)?.booleanOrNull ?: false,
                )
            }

            val actual = RecipeInterpreter.geometry(recipe, shape)

            assertEquals("$label: op sequence", expected.map { it.op }, actual.map { it.op })
            expected.zip(actual).forEach { (want, got) ->
                assertEquals("$label: ${want.op} closed", want.closed, got.closed)
                assertEquals("$label: ${want.op} point count", want.points.size, got.points.size)
                want.points.zip(got.points).forEachIndexed { index, (w, g) ->
                    assertEquals("$label: ${want.op} point $index x", w.x, g.x, tolerance)
                    assertEquals("$label: ${want.op} point $index y", w.y, g.y, tolerance)
                }
            }
        }
    }

    /** Every shipped built-in must decode, or it silently stops drawing. */
    @Test
    fun `every built-in primitive decodes`() {
        val builtins = builtinDirectory().listFiles { file -> file.extension == "json" }
            ?.sortedBy { it.name }
            .orEmpty()
        assertTrue("no built-in primitives found", builtins.isNotEmpty())

        builtins.forEach { file ->
            val recipe = TutorRecipe.from(json.parseToJsonElement(file.readText()))
            assertTrue("${file.name} did not decode", recipe != null)
            assertTrue("${file.name} declares no draw ops", recipe!!.draw.isNotEmpty())
            assertTrue("${file.name} has an empty type", recipe.type.isNotBlank())
        }
    }

    /**
     * Walk up from the module to the repository root. Gradle's working
     * directory for a unit test is the module, and hard-coding the number of
     * parents breaks the moment the module moves.
     */
    private fun repositoryRoot(): File {
        var directory: File? = File(System.getProperty("user.dir") ?: ".").absoluteFile
        while (directory != null) {
            if (File(directory, "docs/components/magician/tutor-primitive-fixtures").isDirectory) {
                return directory
            }
            directory = directory.parentFile
        }
        error("could not locate the repository root from ${System.getProperty("user.dir")}")
    }

    private fun fixtureDirectory() =
        File(repositoryRoot(), "docs/components/magician/tutor-primitive-fixtures")

    private fun builtinDirectory() =
        File(repositoryRoot(), "magician_data_v3/system/tutor_primitives")
}
