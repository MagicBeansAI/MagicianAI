package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.apps.AppWidgetRenderRow
import kotlinx.serialization.json.JsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class AppWidgetSurfaceContractTest {
    @Test
    fun `shell binds complete home and observe native app pages`() {
        assertEquals(NativeAppPageBinding("/", listOf("primary", "secondary")), Destination.Today.nativeAppPageBinding())
        assertEquals(NativeAppPageBinding("/observe", listOf("reviews")), Destination.Observe.nativeAppPageBinding())
        assertEquals(null, Destination.Chat.nativeAppPageBinding())
    }

    @Test
    fun `tree depth is bounded and cycle safe`() {
        val rows = listOf(
            row("root", ""),
            row("child", "root"),
            row("grandchild", "child"),
            row("cycle-a", "cycle-b"),
            row("cycle-b", "cycle-a"),
        )

        val depths = widgetTreeDepths(rows, "parent")

        assertEquals(0, depths["root"])
        assertEquals(1, depths["child"])
        assertEquals(2, depths["grandchild"])
        assertTrue((depths["cycle-a"] ?: 9) <= 8)
        assertTrue((depths["cycle-b"] ?: 9) <= 8)
    }

    private fun row(id: String, parent: String) = AppWidgetRenderRow(
        entity = "item",
        recordId = id,
        recordRevision = 1,
        fields = if (parent.isEmpty()) emptyMap() else mapOf("parent" to JsonPrimitive(parent)),
    )
}
