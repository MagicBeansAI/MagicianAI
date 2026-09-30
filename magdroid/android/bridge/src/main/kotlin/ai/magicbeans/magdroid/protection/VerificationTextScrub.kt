package ai.magicbeans.magdroid.protection

import ai.magicbeans.magdroid.notification.OtpWatcher
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject

/**
 * Hold a verification code out of a screen read.
 *
 * `android_get_notifications` already refuses to read a code out, for a
 * reason it states plainly: the code answers the ask through the custody lane
 * and must never enter a tool result. That reasoning is about the code, not
 * about the tool — and the notification shade is not an app, so the
 * foreground gate never covered it. The same message the notification tool
 * withheld came back verbatim from `android_get_ui_tree` as
 * `text="Your verification code is …"`, which a paired handset demonstrated.
 * A heads-up banner puts that text over whatever app is open, so the test
 * belongs on the content rather than on which surface is showing.
 *
 * The judgement is [OtpWatcher.extractCode] — the same classifier the
 * notification tool uses, so the two surfaces cannot disagree about what a
 * code is.
 */
object VerificationTextScrub {

    /** What replaces a value that carries a code. */
    const val WITHHELD: String =
        "[verification message withheld — answer the ask with android_await_otp]"

    data class Result(val text: String, val withheld: Boolean)

    /**
     * Does this value carry a one-time code?
     *
     * Public because pixels need the same answer as text: a screenshot of a
     * code is the code, and the region to black out is chosen by asking this
     * of each accessibility node. One judgement, so a screen read and a screen
     * capture cannot disagree about what a code is.
     */
    fun carriesACode(value: String?): Boolean =
        value != null &&
            value.any { it.isDigit() } &&
            OtpWatcher.extractCode(value) !is OtpWatcher.Extraction.None

    /**
     * Scrub one tool-result payload, JSON or not.
     *
     * A long value is judged line by line: the Apps snapshot carries its whole
     * element table in a single string, and holding the table because one row
     * carried a code would take the screen away in order to protect one line
     * of it.
     */
    fun scrub(text: String): Result {
        if (text.isEmpty() || text.none { it.isDigit() }) return Result(text, false)
        var withheld = false

        fun scrubValue(value: String): String {
            if (value.none { it.isDigit() }) return value
            if (value.contains('\n')) {
                return value.lineSequence().joinToString("\n") { line ->
                    if (carriesACode(line)) {
                        withheld = true
                        WITHHELD
                    } else {
                        line
                    }
                }
            }
            return if (carriesACode(value)) {
                withheld = true
                WITHHELD
            } else {
                value
            }
        }

        fun redact(element: JsonElement): JsonElement = when (element) {
            is JsonObject -> buildJsonObject {
                element.forEach { (key, value) -> put(key, redact(value)) }
            }
            is JsonArray -> buildJsonArray { element.forEach { add(redact(it)) } }
            is JsonPrimitive ->
                if (element.isString) JsonPrimitive(scrubValue(element.content)) else element
            else -> element
        }

        val scrubbed = try {
            redact(Json.parseToJsonElement(text)).toString()
        } catch (_: Throwable) {
            scrubValue(text)
        }
        return if (withheld) Result(scrubbed, true) else Result(text, false)
    }
}
