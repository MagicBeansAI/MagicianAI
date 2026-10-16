package ai.magicbeans.magdroid.tutor

import kotlin.math.PI
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * The recipe expression grammar, against `RecipeExpressionTests.swift`.
 *
 * The undefined-value rules are the substance. An identifier a shape did not
 * carry has to make its op *skip*, not resolve to zero — a rectangle at zero is
 * a mark in the corner of the lesson, and it is drawn with total confidence.
 */
class RecipeExpressionTest {

    private fun e(expr: String, env: Map<String, Double> = emptyMap()): Double? =
        RecipeExpression.evaluate(expr) { env[it] }

    @Test
    fun `number literals and arithmetic`() {
        assertEquals(28.0, e("28"))
        assertEquals(5.0, e("2+3"))
        assertEquals(6.0, e("10-4"))
        assertEquals(42.0, e("6*7"))
        assertEquals(5.0, e("20/4"))
        assertEquals(4.0, e("1.5+2.5"))
    }

    @Test
    fun `precedence and parentheses`() {
        assertEquals(14.0, e("2+3*4"))
        assertEquals(20.0, e("(2+3)*4"))
        assertEquals(5.0, e("10-2-3"))
        assertEquals(1.0, e("20/4/5"))
    }

    @Test
    fun `unary minus`() {
        assertEquals(-5.0, e("-5"))
        assertEquals(1.0, e("3+-2"))
        assertEquals(-5.0, e("-(2+3)"))
    }

    @Test
    fun `identifiers resolve, and an undefined one poisons its term`() {
        assertEquals(38.0, e("x+size", mapOf("x" to 10.0, "size" to 28.0)))
        assertEquals(20.0, e("r*0.5", mapOf("r" to 40.0)))
        assertNull(e("missing"))
        assertNull(e("x+missing", mapOf("x" to 10.0)))
    }

    @Test
    fun `coalesce takes the first defined operand`() {
        assertEquals(5.0, e("cx|x", mapOf("x" to 5.0)))
        assertEquals(9.0, e("cx|x", mapOf("cx" to 9.0, "x" to 5.0)))
        assertEquals(3.0, e("a|b|c", mapOf("c" to 3.0)))
        assertNull(e("a|b"))
    }

    @Test
    fun `coalesce is the lowest precedence`() {
        assertEquals(36.0, e("r|size", mapOf("size" to 36.0)))
        // Additive binds tighter: (x+1) | (y).
        assertEquals(5.0, e("x+1|y", mapOf("x" to 4.0)))
    }

    @Test
    fun `the function set`() {
        assertEquals(3.0, e("sqrt(9)"))
        assertEquals(7.0, e("abs(-7)"))
        assertEquals(3.0, e("min(3,8)"))
        assertEquals(8.0, e("max(3,8)"))
        assertEquals(260.0, e("min(260,300)"))
        assertEquals(1.0, e("cos(deg(0))")!!, 1e-9)
        assertEquals(1.0, e("sin(deg(90))")!!, 1e-9)
        assertEquals(PI, e("deg(180)")!!, 1e-9)
        assertEquals(90.0, e("rad(deg(90))")!!, 1e-9)
    }

    @Test
    fun `functions nest and compose with arithmetic`() {
        assertEquals(36.0, e("cos(deg(a))*r", mapOf("a" to 0.0, "r" to 36.0))!!, 1e-9)
    }

    /** The shape the shipped callout's background width is computed with. */
    @Test
    fun `the callout width clamp`() {
        assertEquals(52.0, e("min(260,text_len*9+16)", mapOf("text_len" to 4.0)))
        assertEquals(260.0, e("min(260,text_len*9+16)", mapOf("text_len" to 100.0)))
    }

    @Test
    fun `malformed input is undefined rather than guessed at`() {
        assertNull(e("2+"))
        assertNull(e("(3"))
        assertNull(e("*4"))
        assertNull(e("sqrt()"))
        assertNull(e(""))
        // Trailing garbage is not silently ignored.
        assertNull(e("2 3"))
    }

    /**
     * A degenerate result is undefined, not infinite. Drawn, an infinity lands
     * at the clamped edge of the canvas — a confident mark in the wrong place,
     * which is worse than no mark.
     */
    @Test
    fun `undefined beats a degenerate number`() {
        assertNull(e("sqrt(-1)"))
        assertNull(e("1/0"))
        assertEquals(7.0, e("1/0|7")!!, 1e-9)
    }

    /** Recursion is bounded — these arrive over the network. */
    @Test
    fun `absurd nesting is refused rather than overflowing`() {
        assertNull(e("(".repeat(200) + "1" + ")".repeat(200)))
    }
}
