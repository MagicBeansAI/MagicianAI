package ai.magicbeans.magdroid.ui.vibedev

import ai.magicbeans.magdroid.ui.LocalMagicanFontFamilies
import ai.magicbeans.magdroid.ui.StatusDot
import ai.magicbeans.magdroid.ui.activePalette
import ai.magicbeans.magdroid.vibedev.VibeDevCrew
import ai.magicbeans.magdroid.vibedev.VibeDevCrewDecision
import ai.magicbeans.magdroid.vibedev.VibeDevCrewLane
import ai.magicbeans.magdroid.vibedev.VibeDevCrewQuestion
import ai.magicbeans.magdroid.vibedev.WorkItemIds
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Groups
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.semantics
import androidx.compose.foundation.layout.wrapContentHeight
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

internal fun crewStatusLabel(status: String): String = when (status) {
    "proposed" -> "Waiting to start"
    "running" -> "Running"
    "choosing" -> "Pick an attempt"
    "ready" -> "Merging lanes"
    "integrated" -> "Merged"
    "conflict" -> "Lanes conflict"
    "failed" -> "Failed"
    "cancelled" -> "Stopped"
    else -> status.replaceFirstChar { it.uppercase() }
}

private fun laneStatusLabel(status: String): String = when (status) {
    "pending" -> "Waiting"
    "running" -> "Running"
    "done" -> "Done"
    "failed" -> "Failed"
    "stopped" -> "Stopped"
    else -> status
}

/**
 * A VibeDev crew: goal, status, spend of budget, one row per lane, and the
 * question it is waiting on as buttons. "VibeDev crew" and the group icon keep
 * it apart from the Work tab's agent crews.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun VibeDevCrewCard(
    crew: VibeDevCrew,
    projectName: String?,
    question: VibeDevCrewQuestion?,
    busy: Boolean,
    onDecide: (VibeDevCrewDecision) -> Unit,
    onOpenRun: (runId: String, title: String) -> Unit,
) {
    val uri = LocalUriHandler.current
    val fonts = LocalMagicanFontFamilies.current
    VibeCard {
        Row(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
            LabeledDot(if (question != null) crewTone("proposed") else crewTone(crew.status), crewStatusLabel(crew.status))
            Icon(Icons.Outlined.Groups, contentDescription = null, tint = activePalette.accent, modifier = Modifier.size(15.dp))
            Text(
                (if (crew.isRace) "VibeDev race" else "VibeDev crew") + (projectName?.let { " · $it" } ?: ""),
                color = activePalette.accent, fontSize = 12.sp, fontWeight = FontWeight.SemiBold, fontFamily = fonts.body,
                maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f, fill = false),
            )
            MetaText(crewStatusLabel(crew.status), maxLines = 1)
        }
        if (question != null) {
            CardTitle(question.title)
            MetaText(question.body, maxLines = 2)
        } else {
            CardTitle(crew.goal)
        }
        Column(Modifier.padding(top = 2.dp)) {
            crew.lanes.forEach { lane -> LaneRow(lane, onOpenRun) }
        }
        crew.budgetUsd?.let { budget ->
            val spent = crew.spentUsd ?: 0.0
            MetaText("${money(spent)} of ${money(budget)}")
            LinearProgressIndicator(
                progress = { if (budget > 0) (spent / budget).toFloat().coerceIn(0f, 1f) else 0f },
                modifier = Modifier.fillMaxWidth(),
                color = activePalette.accent,
                trackColor = activePalette.soft,
            )
        } ?: crew.spentUsd?.let { MetaText("Spent ${money(it)}") }
        crew.branch?.let { MetaText("Branch $it", mono = true, maxLines = 1) }
        crew.conflict?.let { conflict ->
            MetaText("${conflict.lane} conflicts: ${conflict.files.joinToString(", ")}", mono = true, maxLines = 3)
        }
        crew.pullRequestUrl?.let { url ->
            Text(
                "Open the pull request",
                Modifier.heightIn(min = 48.dp).clickable(role = Role.Button) { runCatching { uri.openUri(url) } }
                    .wrapContentHeight().padding(vertical = 4.dp),
                color = activePalette.accent, fontSize = 13.sp, fontWeight = FontWeight.SemiBold, fontFamily = fonts.body,
            )
        }
        if (question == null && crew.status != "running") crew.note?.let { MetaText(it) }
        val options = question?.options.orEmpty()
        val mergeAgain = question == null && crew.status == "conflict"
        if (options.isNotEmpty() || mergeAgain) {
            FlowRow(
                Modifier.padding(top = 4.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                options.forEach { option ->
                    VibeButton(
                        option.label,
                        onClick = { onDecide(option.decision) },
                        kind = when {
                            option.primary -> VibeButtonKind.Primary
                            option.decision.isDestructive -> VibeButtonKind.Danger
                            else -> VibeButtonKind.Secondary
                        },
                        enabled = !busy,
                    )
                }
                if (mergeAgain) VibeButton("Merge again", { onDecide(VibeDevCrewDecision.MergeAgain) }, enabled = !busy)
            }
        }
        // Until it settles: the crew's own ongoing notification.
        if (crew.status !in CREW_SETTLED) WorkFollowButton(WorkItemIds.crew(crew.crewId), Modifier.padding(top = 4.dp))
    }
}

private val CREW_SETTLED = setOf("integrated", "failed", "cancelled")

@Composable
private fun LaneRow(lane: VibeDevCrewLane, onOpenRun: (String, String) -> Unit) {
    val run = lane.runTaskId
    Row(
        Modifier.fillMaxWidth().heightIn(min = 48.dp)
            // Read as one item: "Integrator, API, Running".
            .semantics(mergeDescendants = true) {}
            .then(if (run != null) Modifier.clickable(role = Role.Button, onClickLabel = "Open the lane's run") { onOpenRun(run, lane.lane) } else Modifier)
            .padding(vertical = 4.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        StatusDot(crewTone(lane.status))
        when (lane.role) {
            "integrate" -> Tag("Integrator")
            "review" -> Tag("Reviewer")
        }
        Text(lane.lane, Modifier.weight(1f), color = activePalette.text, fontSize = 13.sp,
            fontFamily = LocalMagicanFontFamilies.current.body, maxLines = 1, overflow = TextOverflow.Ellipsis)
        MetaText(laneStatusLabel(lane.status), maxLines = 1)
    }
}
