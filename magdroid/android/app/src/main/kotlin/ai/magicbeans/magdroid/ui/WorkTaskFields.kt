package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.tasks.*
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import kotlinx.serialization.json.*

@Composable
internal fun TaskThreadMove(task: TaskV3, model: TasksViewModel, busy: Boolean, onMoved: () -> Unit) {
    var expanded by remember(task.id) { mutableStateOf(false) }
    var selected by remember(task.id) { mutableStateOf("") }
    var search by remember(task.id) { mutableStateOf("") }
    WorkLink("Move to another thread") { expanded = !expanded }
    if (expanded) {
        val resource = rememberWorkResource("/api/magician/v2/ui-threads")
        val state by resource.state.collectAsState()
        val choices = ((state.value as? JsonObject)?.get("threads") as? JsonArray)?.mapNotNull { element ->
            val row = element as? JsonObject ?: return@mapNotNull null
            val id = (row["id"] as? JsonPrimitive)?.contentOrNull ?: return@mapNotNull null
            if (id.isBlank() || (row["archived"] as? JsonPrimitive)?.booleanOrNull == true) null else TaskThreadChoice(id, (row["name"] as? JsonPrimitive)?.contentOrNull ?: id)
        }.orEmpty()
        LaunchedEffect(choices) { if (choices.none { it.id == selected }) selected = "" }
        val matches = choices.filter { it.name.contains(search.trim(), ignoreCase = true) || it.id.contains(search.trim(), ignoreCase = true) }
        Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            WorkResourceStatus(state, resource)
            MagicianTextField(search, { search = it }, Modifier.fillMaxWidth(), label = { Text("Find destination thread") }, singleLine = true)
            choices.firstOrNull { it.id == selected }?.let { Text("Selected: " + it.name, color = Ink) }
            matches.take(20).forEach { choice -> WorkLink(choice.name) { selected = choice.id } }
            if (matches.size > 20) Text("Showing 20 of ${matches.size} matches. Refine the search to find a thread.", color = Muted)
            if (choices.isNotEmpty() && matches.isEmpty()) Text("No matching active threads.", color = Muted)
            if (!state.loading && state.error == null && choices.isEmpty()) Text("No active threads available.", color = Muted)
            WorkLink("Refresh threads", resource::refresh)
            Text("The destination is checked again before the move.", color = Muted)
            OutlinedButton(shape = MagicanButtonShape, onClick = { model.moveTask(task, selected) { if (it) onMoved() } }, enabled = !busy && choices.any { it.id == selected } && selected != task.uiThreadId, modifier = Modifier.fillMaxWidth()) { Text("Move task") }
        }
    }
}
