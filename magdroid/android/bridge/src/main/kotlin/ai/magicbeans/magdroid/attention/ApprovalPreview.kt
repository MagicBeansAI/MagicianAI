package ai.magicbeans.magdroid.attention

import ai.magicbeans.magdroid.vibedev.DiffHunk
import ai.magicbeans.magdroid.vibedev.DiffLine
import ai.magicbeans.magdroid.vibedev.DiffLineKind
import ai.magicbeans.magdroid.vibedev.UnifiedDiff
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.longOrNull

/*
 * A confirmation's structured approval card (`input_schema.approval_preview`,
 * Rust `execution::approval_preview::ApprovalPreview`; web
 * `readApprovalPreview` in `hitl/adapters.ts`). Built by the gated tool when
 * the pause is created, display only: answering posts the same confirmation
 * either way. `why`, `evidence` and the diff lines are agent-authored plain
 * text, never markup. Every field is optional on the wire; a preview without a
 * title is no preview, and the ask then reads as before.
 */

/** One line of a hunk: `context`, `add` or `remove`, its text verbatim. */
data class ApprovalPreviewLine(val kind: Kind, val text: String) {
    enum class Kind { Context, Add, Remove }
}

data class ApprovalPreviewHunk(
    val oldStart: Int,
    val oldLines: Int,
    val newStart: Int,
    val newLines: Int,
    val lines: List<ApprovalPreviewLine>,
) {
    /** `@@ -a,b +c,d @@`, as a unified diff writes it. */
    val header: String get() = "@@ -$oldStart,$oldLines +$newStart,$newLines @@"
}

/** A capped line diff of the text an approval would change. */
data class ApprovalPreviewDiff(
    val path: String,
    /** Totals for the whole change, not only the shown hunks. */
    val added: Int,
    val removed: Int,
    val hunks: List<ApprovalPreviewHunk>,
    val truncated: Boolean,
    /** Changed lines past the cap. */
    val omittedChangedLines: Int,
    val unifiedDiff: String,
) {
    /** `+N lines, −M lines`. */
    val changeLabel: String get() = "+$added ${lines(added)}, −$removed ${lines(removed)}"

    /** "N more changed lines not shown", when the cap cut the diff short. */
    val omittedLabel: String?
        get() = if (truncated && omittedChangedLines > 0) {
            "$omittedChangedLines more changed ${lines(omittedChangedLines)} not shown"
        } else {
            null
        }

    private fun lines(count: Int) = if (count == 1) "line" else "lines"

    /**
     * The shown hunks in the shape the shared diff view draws, numbered from
     * each hunk's starts: context advances both sides, a removal the old, an
     * addition the new.
     */
    fun toUnifiedDiff(): UnifiedDiff = UnifiedDiff(
        header = listOf(path),
        hunks = hunks.map { hunk ->
            var old = hunk.oldStart
            var new = hunk.newStart
            DiffHunk(
                header = hunk.header,
                oldStart = hunk.oldStart, oldCount = hunk.oldLines,
                newStart = hunk.newStart, newCount = hunk.newLines,
                section = "",
                lines = hunk.lines.map { line ->
                    when (line.kind) {
                        ApprovalPreviewLine.Kind.Context -> DiffLine(DiffLineKind.Context, line.text, old++, new++)
                        ApprovalPreviewLine.Kind.Remove -> DiffLine(DiffLineKind.Removed, line.text, old++, null)
                        ApprovalPreviewLine.Kind.Add -> DiffLine(DiffLineKind.Added, line.text, null, new++)
                    }
                },
            )
        },
    )
}

data class ApprovalPreviewWarning(val code: String, val message: String)

data class ApprovalPreview(
    val title: String,
    val whyLabel: String?,
    val why: String?,
    val diff: ApprovalPreviewDiff?,
    val evidence: List<String>,
    val warnings: List<ApprovalPreviewWarning>,
    val notes: List<String>,
) {
    /** The label over [why]; the agent's own words unless the builder named them. */
    val whyHeading: String get() = whyLabel ?: "Why (agent's words)"

    companion object {
        /** Read leniently: a malformed preview is no preview, never a failed feed. */
        fun parse(raw: JsonElement?): ApprovalPreview? {
            val record = raw as? JsonObject ?: return null
            val title = record.string("title")?.trim()?.takeIf(String::isNotEmpty) ?: return null
            return ApprovalPreview(
                title = title,
                whyLabel = record.string("why_label")?.trim()?.takeIf(String::isNotEmpty),
                why = record.string("why")?.takeIf(String::isNotBlank),
                diff = parseDiff(record["diff"]),
                evidence = record.strings("evidence"),
                warnings = (record["warnings"] as? JsonArray).orEmpty().mapNotNull { entry ->
                    val warning = entry as? JsonObject ?: return@mapNotNull null
                    val message = warning.string("message")?.trim()?.takeIf(String::isNotEmpty) ?: return@mapNotNull null
                    ApprovalPreviewWarning(warning.string("code")?.trim()?.takeIf(String::isNotEmpty) ?: "warning", message)
                },
                notes = record.strings("notes"),
            )
        }

        private fun parseDiff(raw: JsonElement?): ApprovalPreviewDiff? {
            val record = raw as? JsonObject ?: return null
            val path = record.string("path")?.trim()?.takeIf(String::isNotEmpty) ?: return null
            val hunks = (record["hunks"] as? JsonArray).orEmpty().mapNotNull { entry ->
                val hunk = entry as? JsonObject ?: return@mapNotNull null
                ApprovalPreviewHunk(
                    oldStart = hunk.count("old_start"),
                    oldLines = hunk.count("old_lines"),
                    newStart = hunk.count("new_start"),
                    newLines = hunk.count("new_lines"),
                    lines = (hunk["lines"] as? JsonArray).orEmpty().mapNotNull { item ->
                        val line = item as? JsonObject ?: return@mapNotNull null
                        // Text is kept verbatim: indentation matters in a diff.
                        val text = line.string("text") ?: return@mapNotNull null
                        val kind = when (line.string("kind")) {
                            "context" -> ApprovalPreviewLine.Kind.Context
                            "add" -> ApprovalPreviewLine.Kind.Add
                            "remove" -> ApprovalPreviewLine.Kind.Remove
                            else -> return@mapNotNull null
                        }
                        ApprovalPreviewLine(kind, text)
                    },
                )
            }
            return ApprovalPreviewDiff(
                path = path,
                added = record.count("added"),
                removed = record.count("removed"),
                hunks = hunks,
                truncated = (record["truncated"] as? JsonPrimitive)?.booleanOrNull ?: false,
                omittedChangedLines = record.count("omitted_changed_lines"),
                unifiedDiff = record.string("unified_diff").orEmpty(),
            )
        }

        private fun JsonObject.string(key: String): String? =
            (get(key) as? JsonPrimitive)?.takeIf { it.isString }?.content

        private fun JsonObject.strings(key: String): List<String> =
            (get(key) as? JsonArray).orEmpty().mapNotNull { entry ->
                (entry as? JsonPrimitive)?.takeIf { it.isString }?.content?.takeIf(String::isNotBlank)
            }

        private fun JsonObject.count(key: String): Int =
            (get(key) as? JsonPrimitive)?.longOrNull?.coerceIn(0L, Int.MAX_VALUE.toLong())?.toInt() ?: 0
    }
}

/**
 * The original call behind a previewed confirmation (`action_summary`, then
 * `reason`), kept for the collapsed audit view. Null when neither is text.
 */
fun approvalRawCall(actionSummary: JsonElement?, reason: JsonElement?): String? =
    listOf(actionSummary, reason)
        .mapNotNull { (it as? JsonPrimitive)?.takeIf { value -> value.isString }?.content?.trim()?.takeIf(String::isNotEmpty) }
        .distinct()
        .joinToString("\n\n")
        .takeIf(String::isNotEmpty)
