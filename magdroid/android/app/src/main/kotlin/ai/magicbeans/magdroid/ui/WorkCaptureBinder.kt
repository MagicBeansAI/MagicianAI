package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.work.*
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
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
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        item { Text("Bind this accepted artifact to a reviewed campaign. Captures are local records; nothing is sent.", color = Muted, fontSize = 14.sp); WorkLink("Inspect exact accepted artifact") { inspect = true }; WorkResourceStatus(state, resource) }
        items(state.value.workRows("campaigns")) { row ->
            val policy = row.workField("state").workField("campaign")
            WorkCard {
                Text(policy.workField("spec").workText("name").orEmpty(), color = Ink, fontSize = 17.sp)
                WorkRecord(policy)
                policy.workField("spec").workRows("targets").filter { it.workText("task_id") == taskId }.forEach { target ->
                    TextButton(onClick = {
                        val identity = state.identity; val id = policy.workText("id") ?: return@TextButton
                        scope.launch { resource.perform("$WORK_QUALITY_API/campaigns/${WorkRepository.segment(id)}/captures", identity, body = buildJsonObject { put("expected_version", policy.workField("version_hash") ?: JsonNull); put("run_id", runId); put("artifact_hash", hash); put("identity_id", target.workField("identity_id") ?: JsonNull) }) }
                    }, enabled = reviewed && !state.busy && policy.workField("revoked_at") == JsonNull) { Text("Bind local capture · ${target.workText("identity_id")}", color = Coral) }
                }
            }
        }
        item { Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
            TextButton(onClick = { page-- }, enabled = page > 0) { Text("Previous", color = Coral) }; Text("Page ${page + 1}", color = Muted)
            TextButton(onClick = { page++ }, enabled = state.value.workField("pagination").workBool("has_more")) { Text("Next", color = Coral) }
        } }
    }
    if (inspect) Dialog(onDismissRequest = { inspect = false }, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Column(Modifier.fillMaxSize().background(Ground).systemBarsPadding()) {
            WorkLink("‹ Capture") { inspect = false }
            WorkQualityArtifact("$runId/$hash", push, onReviewed = { reviewed = true })
        }
    }
}
