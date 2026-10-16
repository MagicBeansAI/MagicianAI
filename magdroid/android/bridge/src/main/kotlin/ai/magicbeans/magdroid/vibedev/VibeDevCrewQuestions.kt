package ai.magicbeans.magdroid.vibedev

import java.time.Instant
import java.time.OffsetDateTime

/** What the owner can decide on a crew card. */
sealed class VibeDevCrewDecision {
    data object Start : VibeDevCrewDecision()
    data object DontStart : VibeDevCrewDecision()
    data class Keep(val laneIndex: Int, val lane: String) : VibeDevCrewDecision()
    data object KeepNone : VibeDevCrewDecision()
    data object Resolve : VibeDevCrewDecision()
    data object ResolveMyself : VibeDevCrewDecision()
    data object MergeAgain : VibeDevCrewDecision()
    data object OpenPullRequest : VibeDevCrewDecision()
    data object NotNow : VibeDevCrewDecision()

    /** Cancels work, so it waits out the undo window before it is sent. */
    val isDestructive: Boolean get() = this == DontStart || this == KeepNone
}

data class VibeDevCrewOption(
    val label: String,
    val decision: VibeDevCrewDecision,
    val primary: Boolean = false,
)

enum class VibeDevCrewQuestionKind { Start, Choose, Resolve, PullRequest }

/** The question a crew is waiting on, drawn as a Needs-you card. */
data class VibeDevCrewQuestion(
    val kind: VibeDevCrewQuestionKind,
    val title: String,
    val body: String,
    val options: List<VibeDevCrewOption>,
) {
    companion object {
        /**
         * The crew's open question, mirroring the server's `open_questions`:
         * proposed asks to start; a finished race asks which attempt to keep;
         * a conflict with its question still open asks who resolves it; and a
         * merged crew whose pull-request question is open asks about a PR.
         */
        fun of(crew: VibeDevCrew): VibeDevCrewQuestion? = when {
            crew.status == "proposed" -> VibeDevCrewQuestion(
                VibeDevCrewQuestionKind.Start,
                if (crew.isRace) "Start this race?" else "Start this crew?",
                crew.goal,
                listOf(
                    VibeDevCrewOption("Start", VibeDevCrewDecision.Start, primary = true),
                    VibeDevCrewOption("Don't start", VibeDevCrewDecision.DontStart),
                ),
            )
            crew.status == "choosing" -> {
                val keep = crew.lanes.withIndex()
                    .filter { (_, lane) -> lane.role == "work" && lane.status == "done" }
                    .mapIndexed { order, (index, lane) ->
                        VibeDevCrewOption("Keep ${lane.lane}", VibeDevCrewDecision.Keep(index, lane.lane), primary = order == 0)
                    }
                VibeDevCrewQuestion(
                    VibeDevCrewQuestionKind.Choose,
                    "Which attempt do you keep?",
                    crew.goal,
                    keep + VibeDevCrewOption("Keep none", VibeDevCrewDecision.KeepNone),
                )
            }
            crew.status == "conflict" && crew.resolveRequestId != null -> VibeDevCrewQuestion(
                VibeDevCrewQuestionKind.Resolve,
                "The lanes conflict",
                crew.conflict?.let { conflict ->
                    val files = conflict.files.take(3).joinToString(", ")
                    "${conflict.lane} conflicts" + if (files.isNotEmpty()) " in $files." else "."
                } ?: crew.goal,
                listOf(
                    VibeDevCrewOption("Resolve with an agent", VibeDevCrewDecision.Resolve, primary = true),
                    VibeDevCrewOption("I'll resolve it", VibeDevCrewDecision.ResolveMyself),
                ),
            )
            crew.pullRequestRequestId != null && crew.pullRequestUrl == null -> VibeDevCrewQuestion(
                VibeDevCrewQuestionKind.PullRequest,
                "Merged. Open a pull request?",
                buildString {
                    append("All lanes are in ").append(crew.branch ?: "the crew branch").append('.')
                    crew.spentUsd?.let { append(" Spent ").append(usd(it)).append('.') }
                },
                listOf(
                    VibeDevCrewOption("Open pull request", VibeDevCrewDecision.OpenPullRequest, primary = true),
                    VibeDevCrewOption("Not now", VibeDevCrewDecision.NotNow),
                ),
            )
            else -> null
        }
    }
}

/** One card in Now's Needs-you section. */
sealed class VibeDevNeedsYou {
    abstract val key: String
    abstract val sortMs: Long

    data class CrewAsk(val crew: VibeDevCrew, val projectId: String, val projectName: String, val question: VibeDevCrewQuestion) : VibeDevNeedsYou() {
        override val key: String get() = "crew:${crew.crewId}:${question.kind}"
        override val sortMs: Long get() = crew.updatedAtMs
    }

    data class RunAsk(val run: VibeDevLiveRun) : VibeDevNeedsYou() {
        override val key: String get() = "run:${run.runId}"
        override val sortMs: Long get() = isoMillis(run.updatedAt) ?: 0
    }

    companion object {
        /**
         * Crew questions first (a whole crew waits on each), then runs waiting
         * on an approval; newest first within each.
         */
        fun order(projects: List<VibeDevProject>, runs: List<VibeDevLiveRun>): List<VibeDevNeedsYou> {
            val crews = projects.filterNot { it.archived }.flatMap { project ->
                project.crews.mapNotNull { crew ->
                    VibeDevCrewQuestion.of(crew)?.let { CrewAsk(crew, project.projectId, project.name, it) }
                }
            }.sortedByDescending { it.sortMs }
            val asks = runs.filter { it.state == VibeDevRunState.NeedsYou }
                .map(::RunAsk)
                .sortedByDescending { it.sortMs }
            return crews + asks
        }
    }
}

internal fun usd(value: Double): String = "$" + String.format(java.util.Locale.US, "%.2f", value)

/** Milliseconds since the epoch for an RFC 3339 timestamp, or null. */
fun isoMillis(text: String?): Long? {
    if (text.isNullOrBlank()) return null
    return runCatching { Instant.parse(text).toEpochMilli() }.getOrNull()
        ?: runCatching { OffsetDateTime.parse(text).toInstant().toEpochMilli() }.getOrNull()
}
