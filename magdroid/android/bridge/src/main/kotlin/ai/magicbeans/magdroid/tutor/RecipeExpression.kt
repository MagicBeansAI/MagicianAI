package ai.magicbeans.magdroid.tutor

import kotlin.math.PI
import kotlin.math.abs
import kotlin.math.cos
import kotlin.math.sin
import kotlin.math.sqrt
import kotlin.math.tan

/**
 * The little arithmetic language a tutor recipe writes its coordinates in.
 *
 * A port of `RecipeExpression.swift`, which is itself identical to the TS and
 * Rust evaluators — the grammar is normative in Appendix A of
 * `docs/archive/plans/2026-07-13-data-driven-tutor-primitives.md`:
 *
 * - number literals, identifiers (resolved through [resolve]), parentheses
 * - binary `+ - * /`, unary `-` and `+`
 * - coalesce `|`, the **lowest** precedence — "first defined operand"
 * - functions `sin cos tan sqrt abs min max deg rad`
 *   (`deg` degrees→radians, `rad` radians→degrees)
 *
 * There is no code execution here, only arithmetic over a fixed function set,
 * and recursion is bounded — the recipes are fetched from a server and a scope
 * can author its own, so this parser reads hostile input by construction.
 *
 * The distinction the whole thing turns on: an undefined identifier *inside* a
 * coalesce chain is skipped, while an undefined identifier *outside* one makes
 * the expression undefined, and its op is then skipped rather than drawn at a
 * clamped boundary. Division by zero is undefined for the same reason — a
 * degenerate op should draw nothing, not a line to infinity.
 */
object RecipeExpression {

    private const val MAX_DEPTH = 32

    /**
     * Evaluate [expr], resolving bare identifiers through [resolve].
     *
     * Returns null for a syntax error, for trailing garbage, or for an
     * undefined result outside a coalesce chain.
     */
    fun evaluate(expr: String, resolve: (String) -> Double?): Double? {
        val parser = Parser(expr, resolve)
        val value = parser.parseCoalesce(0) ?: return null
        parser.skipSpaces()
        if (!parser.isAtEnd) return null
        return value
    }

    /**
     * A parse result. [value] null means "undefined" — the expression parsed
     * but an identifier in it resolved to nothing. [ok] false means the text
     * was not a valid expression at all. Keeping them apart is what lets a
     * coalesce chain skip an undefined operand while still failing on garbage.
     */
    private data class Res(val value: Double?, val ok: Boolean)

    private class Parser(text: String, val resolve: (String) -> Double?) {
        private val chars: CharArray = text.toCharArray()
        private var pos: Int = 0

        val isAtEnd: Boolean get() = pos >= chars.size

        fun skipSpaces() {
            while (pos < chars.size && (chars[pos] == ' ' || chars[pos] == '\t')) pos += 1
        }

        private fun peek(): Char? = if (pos < chars.size) chars[pos] else null

        /** Coalesce: lowest precedence, first defined operand wins. */
        fun parseCoalesce(depth: Int): Double? {
            if (depth > MAX_DEPTH) return null
            var result: Double? = null
            while (true) {
                val (value, ok) = parseAddSub(depth + 1)
                if (!ok) return null
                // Only a finite value counts. An infinity from a degenerate
                // computation is "undefined", not a very large number.
                if (result == null && value != null && value.isFinite()) result = value
                skipSpaces()
                if (peek() == '|') {
                    pos += 1
                    continue
                }
                break
            }
            return result
        }

        fun parseAddSub(depth: Int): Res {
            if (depth > MAX_DEPTH) return Res(null, false)
            var left = parseMulDiv(depth + 1)
            if (!left.ok) return Res(null, false)
            var value = left.value
            while (true) {
                skipSpaces()
                val c = peek()
                if (c != '+' && c != '-') break
                pos += 1
                val right = parseMulDiv(depth + 1)
                if (!right.ok) return Res(null, false)
                val l = value
                val r = right.value
                if (l == null || r == null) {
                    value = null
                    continue
                }
                value = if (c == '+') l + r else l - r
            }
            left = Res(value, true)
            return left
        }

        fun parseMulDiv(depth: Int): Res {
            if (depth > MAX_DEPTH) return Res(null, false)
            val first = parseUnary(depth + 1)
            if (!first.ok) return Res(null, false)
            var value = first.value
            while (true) {
                skipSpaces()
                val c = peek()
                if (c != '*' && c != '/') break
                pos += 1
                val right = parseUnary(depth + 1)
                if (!right.ok) return Res(null, false)
                val l = value
                val r = right.value
                if (l == null || r == null) {
                    value = null
                    continue
                }
                // Dividing by zero is undefined rather than infinite, so the op
                // is skipped instead of drawn at the canvas edge.
                value = if (c == '/') { if (r == 0.0) null else l / r } else l * r
            }
            return Res(value, true)
        }

        fun parseUnary(depth: Int): Res {
            if (depth > MAX_DEPTH) return Res(null, false)
            skipSpaces()
            if (peek() == '-') {
                pos += 1
                val inner = parseUnary(depth + 1)
                if (!inner.ok) return Res(null, false)
                return Res(inner.value?.let { -it }, true)
            }
            if (peek() == '+') {
                pos += 1
                return parseUnary(depth + 1)
            }
            return parsePrimary(depth + 1)
        }

        fun parsePrimary(depth: Int): Res {
            if (depth > MAX_DEPTH) return Res(null, false)
            skipSpaces()
            val c = peek() ?: return Res(null, false)

            if (c == '(') {
                pos += 1
                val value = parseCoalesce(depth + 1)
                skipSpaces()
                if (peek() != ')') return Res(null, false)
                pos += 1
                return Res(value, true)
            }
            if (c.isDigit() || c == '.') return parseNumber()
            if (c.isLetter() || c == '_') return parseIdentifierOrCall(depth)
            return Res(null, false)
        }

        private fun parseNumber(): Res {
            val start = pos
            while (pos < chars.size && (chars[pos].isDigit() || chars[pos] == '.')) pos += 1
            val text = String(chars, start, pos - start)
            val parsed = text.toDoubleOrNull() ?: return Res(null, false)
            return Res(parsed, true)
        }

        private fun parseIdentifierOrCall(depth: Int): Res {
            val start = pos
            while (pos < chars.size && (chars[pos].isLetterOrDigit() || chars[pos] == '_')) pos += 1
            val name = String(chars, start, pos - start)
            skipSpaces()
            if (peek() == '(') {
                pos += 1
                val args = mutableListOf<Double?>()
                skipSpaces()
                if (peek() != ')') {
                    while (true) {
                        args += parseCoalesce(depth + 1)
                        skipSpaces()
                        if (peek() == ',') {
                            pos += 1
                            continue
                        }
                        break
                    }
                }
                skipSpaces()
                if (peek() != ')') return Res(null, false)
                pos += 1
                return Res(applyFunction(name, args), true)
            }
            return Res(resolve(name), true)
        }

        private fun applyFunction(name: String, args: List<Double?>): Double? = when (name) {
            "sin", "cos", "tan", "sqrt", "abs", "deg", "rad" -> {
                val a = args.singleOrNull()
                when {
                    a == null -> null
                    name == "sin" -> sin(a)
                    name == "cos" -> cos(a)
                    name == "tan" -> tan(a)
                    // A negative root is undefined rather than NaN, so the op is
                    // skipped instead of drawing at a coordinate that is not one.
                    name == "sqrt" -> if (a < 0) null else sqrt(a)
                    name == "abs" -> abs(a)
                    name == "deg" -> a * PI / 180
                    else -> a * 180 / PI
                }
            }

            "min", "max" -> {
                if (args.size != 2) {
                    null
                } else {
                    val a = args[0]
                    val b = args[1]
                    if (a == null || b == null) null
                    else if (name == "min") minOf(a, b) else maxOf(a, b)
                }
            }

            else -> null
        }
    }
}
