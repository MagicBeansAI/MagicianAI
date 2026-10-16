package ai.magicbeans.magdroid.tasks

import ai.magicbeans.magdroid.chat.ActivityRow
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.doubleOrNull
import java.math.BigDecimal
import java.math.MathContext
import java.math.RoundingMode
import java.util.Locale

/*
 * Model usage per task and per chat turn: calls, tokens (input + output) and
 * cost. Android port of iOS `TaskUsageModel.swift` (web
 * `chat/next/taskUsage.ts`, the `runCost` reading of `taskTimeline.ts`,
 * `llm/tokenUsage.ts`, and the turn chip / step suffix of
 * `RequestActivityCard.svelte`).
 *
 * Absence and zero are different values throughout: a task nothing reported
 * usage for has no usage (null), never a record of zeros, and a cost stays
 * null until some call carries a backend-recorded price. A surface renders
 * nothing for null — never `0 tok · $0.00`.
 */

/** What a set of model calls cost together. */
data class TaskUsage(
    val calls: Int,
    val tokens: Int,
    /** Null until a call reports a price. */
    val costUsd: Double?,
) {
    /** `this + other`; the cost stays null only when neither side was priced. */
    fun adding(other: TaskUsage): TaskUsage = TaskUsage(
        calls = calls + other.calls,
        tokens = tokens + other.tokens,
        costUsd = if (costUsd == null && other.costUsd == null) null else (costUsd ?: 0.0) + (other.costUsd ?: 0.0),
    )

    companion object {
        val ZERO = TaskUsage(0, 0, null)
    }
}

/** What one `/execution-panel` payload states: its run and its planning. */
data class TaskUsageSnapshot(val run: TaskUsage?, val planning: TaskUsage?)

/** The figure a task shows: run + planning, and the two parts for its accessibility label. */
data class TaskUsageTotal(val usage: TaskUsage, val run: TaskUsage?, val planning: TaskUsage?)

object TaskUsageModel {
    /** `a + b`, where null is "nothing yet" (web `add`). */
    fun add(a: TaskUsage?, b: TaskUsage): TaskUsage = (a ?: TaskUsage.ZERO).adding(b)

    /** The usage an `/execution-panel` payload states (web `usageSnapshotFrom`). */
    fun usageSnapshot(panel: JsonObject): TaskUsageSnapshot =
        TaskUsageSnapshot(run = runUsage(panel), planning = planningUsage(panel))

    /**
     * The run's activity (`run.activity_log`, else `run.recent_activity`)
     * summed the way web `runCost` sums the Run act's rows — a row is a call
     * when it reported any of the four token figures, its tokens are input +
     * output, and its price (`cost_usd`, else the older `cost`) counts only on
     * `llm.*` rows. Null when no row reported tokens or a price.
     */
    fun runUsage(panel: JsonObject): TaskUsage? {
        val run = panel["run"].jObj()
        val log = run?.get("activity_log").jObjects().orEmpty()
        val activity = log.ifEmpty { run?.get("recent_activity").jObjects().orEmpty() }
        var calls = 0
        var tokens = 0
        var reportedTokens = false
        var cost: Double? = null
        for (item in activity) {
            val meta = item["metadata"].jObj() ?: JsonObject(emptyMap())
            val input = number(meta["input_tokens"])
            val output = number(meta["output_tokens"])
            val cacheRead = number(meta["cache_read_tokens"])
            val cacheCreation = number(meta["cache_creation_tokens"])
            if (input != null || output != null || cacheRead != null || cacheCreation != null) {
                reportedTokens = true
                calls += 1
                tokens += ((input ?: 0.0) + (output ?: 0.0)).toInt()
            }
            val eventType = meta["event_type"].jStr() ?: ""
            if (eventType.startsWith("llm.")) {
                val price = number(meta["cost_usd"]) ?: number(meta["cost"])
                if (price != null && price >= 0) cost = (cost ?: 0.0) + price
            }
        }
        if (!reportedTokens && cost == null) return null
        return TaskUsage(calls, tokens, cost)
    }

    /**
     * Each `debug.planning_timeline` row with `tokens` or `cost_usd` is one
     * call — or, for a backfilled summary row, `llm_calls` calls (at least
     * one). An older payload simply has no planning usage.
     */
    fun planningUsage(panel: JsonObject): TaskUsage? {
        val rows = panel["debug"].jObj()?.get("planning_timeline").jObjects().orEmpty()
        var planning: TaskUsage? = null
        for (entry in rows) {
            val tokens = number(entry["tokens"])
            val priced = number(entry["cost_usd"])
            if (tokens == null && priced == null) continue
            val calls = number(entry["llm_calls"])?.let { maxOf(1, it.toInt()) } ?: 1
            planning = add(planning, TaskUsage(calls, (tokens ?: 0.0).toInt(), priced))
        }
        return planning
    }

    /**
     * The task's total: the live run when calls were recorded, else the
     * snapshot's run; planning always from the snapshot (planning calls are
     * not on the task's event stream). Null when nothing reported usage.
     */
    fun total(live: TaskUsage?, snapshot: TaskUsageSnapshot?): TaskUsageTotal? {
        val run = if (live != null && live.calls > 0) live else snapshot?.run
        val planning = snapshot?.planning
        val usage = add(run, planning ?: TaskUsage.ZERO)
        if (usage.calls == 0 && usage.costUsd == null) return null
        return TaskUsageTotal(usage, run, planning)
    }

    /** A finite JSON number, never a boolean or a string. */
    fun number(value: JsonElement?): Double? =
        (value as? JsonPrimitive)?.takeIf { !it.isString && it.booleanOrNull == null }
            ?.doubleOrNull?.takeIf { it.isFinite() }
}

/**
 * Per-task live calls and stored snapshots (web `taskUsage` stores). A plain
 * value-holder so the rules are testable without a store; the host owns one
 * and republishes it on change.
 */
class TaskUsageLedger {
    private val seen = mutableSetOf<String>()
    private val liveByTask = linkedMapOf<String, TaskUsage>()
    private val snapshotsByTask = linkedMapOf<String, TaskUsageSnapshot>()

    val live: Map<String, TaskUsage> get() = liveByTask.toMap()
    val snapshots: Map<String, TaskUsageSnapshot> get() = snapshotsByTask.toMap()

    /** Record one finished model call of a task's run, once per [callKey]. False for a replayed call. */
    fun record(taskId: String, callKey: String, tokens: Int?, costUsd: Double?): Boolean {
        if (!seen.add("$taskId::$callKey")) return false
        liveByTask[taskId] = TaskUsageModel.add(liveByTask[taskId], TaskUsage(1, tokens ?: 0, costUsd))
        return true
    }

    /** Store a task's panel snapshot; false when it is unchanged. */
    fun setSnapshot(snapshot: TaskUsageSnapshot, taskId: String): Boolean {
        if (snapshotsByTask[taskId] == snapshot) return false
        snapshotsByTask[taskId] = snapshot
        return true
    }

    /** A panel the caller already fetched (the live card's poll). */
    fun apply(panel: JsonObject, taskId: String): Boolean =
        setSnapshot(TaskUsageModel.usageSnapshot(panel), taskId)

    fun total(taskId: String): TaskUsageTotal? =
        TaskUsageModel.total(liveByTask[taskId], snapshotsByTask[taskId])
}

/** The turn chip's figures: the turn's own model calls plus every task the turn started. */
data class TurnUsageSummary(
    val calls: Int,
    val inputTokens: Int,
    val outputTokens: Int,
    val costUsd: Double?,
    /** Tasks this turn started (`create_task` / `run_task`), priced or not yet. */
    val spawnedTasks: Int,
) {
    val tokens: Int get() = inputTokens + outputTokens
}

object TurnUsage {
    /**
     * The turn's own calls: every LLM row that finished. A call is counted
     * when it reported any token figure; with none reporting tokens, every
     * finished call counts. Null when no call reported tokens or a price.
     */
    fun own(rows: List<ActivityRow>): TurnUsageSummary? {
        val seenCallIds = mutableSetOf<String>()
        // One logical call delivered by two transports counts once.
        val calls = rows.mapNotNull { it.callUsage }.filter { call -> call.callId?.let(seenCallIds::add) ?: true }
        var input: Int? = null
        var output: Int? = null
        var reported = 0
        var cost: Double? = null
        for (call in calls) {
            if (call.reportedTokens) {
                reported += 1
                call.inputTokens?.let { input = (input ?: 0) + it }
                call.outputTokens?.let { output = (output ?: 0) + it }
            }
            call.costUsd?.let { cost = (cost ?: 0.0) + it }
        }
        if (reported == 0 && cost == null) return null
        return TurnUsageSummary(
            calls = if (reported > 0) reported else calls.size,
            inputTokens = input ?: 0,
            outputTokens = output ?: 0,
            costUsd = cost,
            spawnedTasks = 0,
        )
    }

    /** The turn's own usage plus that of the tasks it started. Task tokens count as output. */
    fun summary(rows: List<ActivityRow>, taskUsage: (String) -> TaskUsageTotal?): TurnUsageSummary? {
        val base = own(rows)
        val taskIds = spawnedTaskIds(rows)
        if (taskIds.isEmpty()) return base
        var summary = base ?: TurnUsageSummary(0, 0, 0, null, 0)
        for (usage in taskIds.mapNotNull(taskUsage).map { it.usage }) {
            summary = summary.copy(
                calls = summary.calls + usage.calls,
                outputTokens = summary.outputTokens + usage.tokens,
                costUsd = usage.costUsd?.let { (summary.costUsd ?: 0.0) + it } ?: summary.costUsd,
            )
        }
        return summary.copy(spawnedTasks = taskIds.size)
    }

    fun spawnedTaskIds(rows: List<ActivityRow>): List<String> = rows.flatMap { it.spawnedTaskIds }.distinct()
}

object TaskUsageFormat {
    /**
     * A backend-recorded USD amount (web `formatUsd`, magios
     * `TaskTimelineFormatting.usd`): three decimal places below a dollar
     * (`$0.005`, `$0.040`, `$0.999`), two from a dollar up (`$1.00`, `$3.23`,
     * `$12.00`), `$0.000` for a zero and `<$0.001` for a positive amount that
     * rounds to zero thousandths. Rounding is half-up, with no thousands
     * separators; an amount that rounds up to a dollar at three places takes
     * the dollar form. Null for no price.
     */
    fun usd(amount: Double?): String? {
        if (amount == null || !amount.isFinite() || amount < 0) return null
        val mills = roundHalfUp(amount, 3)
        if (mills.signum() == 0) return if (amount == 0.0) "$0.000" else "<$0.001"
        if (mills < BigDecimal.ONE) return "$" + mills.toPlainString()
        return "$" + roundHalfUp(amount, 2).toPlainString()
    }

    /** [usd] for TalkBack: `<$0.001` reads "less than a tenth of a cent". */
    fun spokenUsd(amount: Double?): String? {
        val text = usd(amount)
        return if (text == "<$0.001") "less than a tenth of a cent" else text
    }

    /**
     * [amount] rounded half-up to [places] on its decimal value: the scaled
     * product is cleaned to 12 significant digits first (web
     * `toPrecision(12)`), so a binary artefact such as
     * `1.005 * 100 = 100.49999…` still rounds up.
     */
    private fun roundHalfUp(amount: Double, places: Int): BigDecimal =
        BigDecimal(amount).movePointRight(places).round(MathContext(12, RoundingMode.HALF_UP))
            .setScale(0, RoundingMode.HALF_UP).movePointLeft(places)

    /** The live card's and planning activity's tokens: `12.3k` from 1000 up, else the count. */
    fun compactTokens(value: Int): String =
        if (value >= 1000) String.format(Locale.US, "%.1fk", value / 1000.0) else value.toString()

    /** The turn chip's and a step's tokens (web `formatTokenCount`): `850`, `9.1k`, `42k`, `1.2M`. */
    fun tokenCount(value: Int): String = when {
        value <= 0 -> "0"
        value < 1000 -> value.toString()
        value < 10_000 -> String.format(Locale.US, "%.1fk", value / 1000.0)
        value < 1_000_000 -> "${Math.round(value / 1000.0)}k"
        else -> String.format(Locale.US, "%.1fM", value / 1_000_000.0)
    }

    /** One call's usage for its step row: `" · 9.1k tok · $0.012"`, or `""` (web `llmUsageSuffix`). */
    fun stepSuffix(tokens: Int?, costUsd: Double?): String =
        listOfNotNull(tokens?.takeIf { it > 0 }?.let { "${tokenCount(it)} tok" }, usd(costUsd))
            .joinToString("") { " · $it" }

    /** A step detail with its usage appended; null when there is neither (web `withUsage`). */
    fun withUsage(detail: String?, suffix: String): String? {
        if (detail.isNullOrEmpty()) return if (suffix.isEmpty()) null else suffix.drop(3)
        return detail + suffix
    }

    fun calls(count: Int): String = "$count call${if (count == 1) "" else "s"}"

    /** The live card's line: `3 calls · 12.3k tok · $0.04`; null with no calls. */
    fun taskLine(usage: TaskUsage): String? {
        if (usage.calls <= 0) return null
        return listOfNotNull(calls(usage.calls), "${compactTokens(usage.tokens)} tok", usd(usage.costUsd))
            .joinToString(" · ")
    }

    /** The live card's spoken figure, split into Planning and Run. */
    fun taskAccessibilityLabel(total: TaskUsageTotal): String {
        val usage = total.usage
        val parts = mutableListOf(
            "${usage.calls} model call${if (usage.calls == 1) "" else "s"}, ${usage.tokens} tokens" +
                (spokenUsd(usage.costUsd)?.let { ", $it" } ?: ""),
        )
        total.planning?.let { parts.add("Planning: ${calls(it.calls)}" + (spokenUsd(it.costUsd)?.let { c -> ", $c" } ?: "")) }
        total.run?.let {
            parts.add("Run, including delegated work: ${calls(it.calls)}" + (spokenUsd(it.costUsd)?.let { c -> ", $c" } ?: ""))
        }
        return parts.joinToString(". ")
    }

    /** The turn chip: `4 calls · 12k tok · $0.04 · incl. 1 task`; null when nothing reported. */
    fun turnLine(summary: TurnUsageSummary): String? {
        if (summary.calls <= 0 && summary.costUsd == null) return null
        val tasks = summary.spawnedTasks.takeIf { it > 0 }?.let { "incl. $it task${if (it == 1) "" else "s"}" }
        return listOfNotNull(calls(summary.calls), "${tokenCount(summary.tokens)} tok", usd(summary.costUsd), tasks)
            .joinToString(" · ")
    }

    /** The turn chip's short form for a narrow header: `12k tok · $0.04`. */
    fun turnShortLine(summary: TurnUsageSummary): String? {
        if (summary.calls <= 0 && summary.costUsd == null) return null
        return listOfNotNull("${tokenCount(summary.tokens)} tok", usd(summary.costUsd)).joinToString(" · ")
    }

    fun turnAccessibilityLabel(summary: TurnUsageSummary): String {
        var label = "${summary.calls} model call${if (summary.calls == 1) "" else "s"}, ${summary.tokens} tokens"
        spokenUsd(summary.costUsd)?.let { label += ", $it" }
        if (summary.spawnedTasks > 0) {
            label += ", including ${summary.spawnedTasks} task${if (summary.spawnedTasks == 1) "" else "s"} this turn started"
        }
        return label
    }

    /** The planning activity toggle's usage: `" · 36.0k tok · $0.050"`, or `""`. */
    fun planningSuffix(entries: List<PlanningEntry>): String {
        val tokens = entries.sumOf { it.tokens ?: 0 }
        val priced = entries.mapNotNull { it.costUsd }
        val cost = if (priced.isEmpty()) null else usd(priced.sum())
        return listOfNotNull(tokens.takeIf { it > 0 }?.let { "${compactTokens(it)} tok" }, cost)
            .joinToString("") { " · $it" }
    }

    /** A Run act summary with the run's usage: `… · 5 calls · 9.1k tok · $0.03`. */
    fun runSummary(summary: String, run: TaskUsage?): String {
        val line = run?.let(::taskLine) ?: return summary
        return if (summary.isEmpty()) line else "$summary · $line"
    }
}
