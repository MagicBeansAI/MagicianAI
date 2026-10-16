package ai.magicbeans.magdroid.ui

import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.AccountTree
import androidx.compose.material3.Icon
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

/**
 * The small plan glyph before a planned task's title (Tasks list row, task
 * detail header and compact title): the plan-graph icon in the theme's
 * secondary accent. Whether a task is planned is `TaskPlanMarker.hasPlan`.
 */
@Composable
fun TaskPlanGlyph(modifier: Modifier = Modifier, size: Dp = TASK_PLAN_GLYPH_DP.dp) {
    Icon(
        Icons.Outlined.AccountTree,
        contentDescription = "Planned task",
        tint = activePalette.secondaryAccentColor,
        modifier = modifier.size(size),
    )
}

internal const val TASK_PLAN_GLYPH_DP = 13
