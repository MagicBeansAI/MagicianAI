package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.work.*
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch
import kotlinx.serialization.json.*

@Composable
internal fun WorkCaptureBinder(runId: String, hash: String, taskId: String, push: (WorkRoute) -> Unit) {
    var page by remember { mutableIntStateOf(0) }; var inspect by remember { mutableStateOf(false) }; var reviewed by remember { mutableStateOf(false) }
    val resource = rememberWorkResource("$WORK_QUALITY_API/campaigns?limit=10&offset=${page * 10}")
    val state by resource.state.collectAsStateWithLifecycle(); val scope = rememberCoroutineScope()
    LaunchedEffect(state.identity) { reviewed = false; inspect = false }
    val campaigns = state.value.workRows("campaigns")
    WorkPage {
        item { Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            WorkGroup { WorkRow("Inspect exact accepted artifact", icon = Icons.Outlined.FindInPage, value = if (reviewed) "Reviewed" else null) { inspect = true } }
            WorkFootnote("Bind this accepted artifact to a reviewed campaign. Captures are local records; nothing is sent.")
        } }
        workStatusItem(state, resource)
        if (state.value != null && campaigns.isEmpty()) item { WorkGroup { WorkEmptyState("No capture policies to bind.", icon = Icons.Outlined.Policy) } }
        items(campaigns) { row ->
            val policy = row.workField("state").workField("campaign")
            WorkHeaderCard(policy.workField("spec").workText("name").orEmpty(), subtitle = policy.workText("id")) {
                policy.workField("spec").workRows("targets").filter { it.workText("task_id") == taskId }.forEach { target ->
                    WorkActionRow("Bind local capture · ${target.workText("identity_id")}", Icons.Outlined.Link, enabled = reviewed && !state.busy && policy.workField("revoked_at") == JsonNull) {
                        val identity = state.identity; val id = policy.workText("id") ?: return@WorkActionRow
                        scope.launch { resource.perform("$WORK_QUALITY_API/campaigns/${WorkRepository.segment(id)}/captures", identity, body = buildJsonObject { put("expected_version", policy.workField("version_hash") ?: JsonNull); put("run_id", runId); put("artifact_hash", hash); put("identity_id", target.workField("identity_id") ?: JsonNull) }) }
                    }
                    WorkRowDivider()
                }
                WorkRecordRow("Policy record", policy, icon = Icons.Outlined.Article)
            }
        }
        if (state.value != null) item { WorkPager(page, state.value.workField("pagination").workBool("has_more"), { page = it }) }
    }
    if (inspect) Dialog(onDismissRequest = { inspect = false }, properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false)) {
        Column(Modifier.fillMaxSize().background(Ground).systemBarsPadding()) {
            WorkDetailBar("Accepted artifact", onBack = { inspect = false }, backLabel = "‹ Capture")
            WorkQualityArtifact("$runId/$hash", push, onReviewed = { reviewed = true })
        }
    }
}
