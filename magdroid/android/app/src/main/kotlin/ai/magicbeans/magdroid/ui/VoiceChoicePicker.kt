package ai.magicbeans.magdroid.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch

internal data class VoiceChoice(val id: String, val label: String, val detail: String? = null, val available: Boolean = true)
internal data class VoiceChoiceRequest(val title: String, val selected: String, val choices: List<VoiceChoice>, val onSelect: suspend (String) -> Unit)
internal class VoiceChoiceHost {
    var request by mutableStateOf<VoiceChoiceRequest?>(null)
}
internal val LocalVoiceChoiceHost = staticCompositionLocalOf<VoiceChoiceHost?> { null }

/** Compact current-value row. Inside voice settings it replaces the sheet's
 * content; standalone Settings uses one modal bottom sheet. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun VoiceChoicePicker(title: String, selected: String, choices: List<VoiceChoice>, enabled: Boolean = true, onSelect: suspend (String) -> Unit) {
    val inherited = LocalVoiceChoiceHost.current
    val fallback = remember { VoiceChoiceHost() }
    val host = inherited ?: fallback
    Surface(color = Panel, shape = RoundedCornerShape(8.dp), border = BorderStroke(1.dp, BorderSoft)) {
        Row(Modifier.fillMaxWidth().alpha(if (enabled) 1f else .45f)
            .clickable(enabled = enabled, role = Role.Button) { host.request = VoiceChoiceRequest(title, selected, choices, onSelect) }
            .padding(horizontal = 12.dp, vertical = 10.dp), verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(3.dp)) {
                Text(title, color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                Text(choices.firstOrNull { it.id == selected }?.label ?: selected.ifEmpty { "Choose…" }, color = Muted, fontSize = 11.sp, maxLines = 1)
            }
            Text("›", color = Muted, fontSize = 20.sp)
        }
    }
    if (inherited == null && fallback.request != null) {
        ModalBottomSheet(onDismissRequest = { fallback.request = null }, containerColor = Ground,
            sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
            VoiceChoicePage(fallback)
        }
    }
}

@Composable
internal fun VoiceChoicePage(host: VoiceChoiceHost) {
    val request = host.request ?: return
    var query by remember(request) { mutableStateOf("") }
    var saving by remember(request) { mutableStateOf(false) }
    var error by remember(request) { mutableStateOf<String?>(null) }
    val scope = rememberCoroutineScope()
    BackHandler { if (!saving) host.request = null }
    Column(Modifier.fillMaxWidth().heightIn(max = 560.dp).padding(horizontal = 16.dp, vertical = 8.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            TextButton(enabled = !saving, onClick = { host.request = null }) { Text("Back", color = Coral) }
            Text(request.title, color = Ink, fontSize = 16.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
            if (saving) CircularProgressIndicator(Modifier.size(18.dp), color = Coral, strokeWidth = 2.dp)
        }
        if (request.choices.size > 10) OutlinedTextField(value = query, onValueChange = { query = it }, label = { Text("Search ${request.title.lowercase()}") },
            singleLine = true, enabled = !saving, modifier = Modifier.fillMaxWidth())
        error?.let { Text(it, color = Danger, fontSize = 12.sp) }
        val filtered = request.choices.filter { "${it.label} ${it.id} ${it.detail.orEmpty()}".contains(query, ignoreCase = true) }
        LazyColumn(Modifier.fillMaxWidth().weight(1f, fill = false)) {
            items(filtered, key = { it.id }) { choice ->
                Row(Modifier.fillMaxWidth().semantics { selected = request.selected == choice.id }
                    .alpha(if (choice.available) 1f else .45f)
                    .clickable(enabled = !saving && choice.available, role = Role.RadioButton) {
                        scope.launch {
                            saving = true
                            try { request.onSelect(choice.id); host.request = null }
                            catch (cancel: CancellationException) { throw cancel }
                            catch (_: Exception) { error = "Could not save. Your previous choice is still selected." }
                            finally { saving = false }
                        }
                    }.padding(vertical = 12.dp, horizontal = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                    Column(Modifier.weight(1f)) {
                        Text(choice.label, color = Ink, fontSize = 14.sp, fontWeight = FontWeight.Medium)
                        choice.detail?.let { Text(it, color = Muted, fontSize = 11.sp) }
                    }
                    if (request.selected == choice.id) Text("✓", color = Coral)
                }
                HorizontalDivider(color = BorderSoft)
            }
            if (filtered.isEmpty()) item { Text("No matches", color = Muted, modifier = Modifier.padding(12.dp)) }
        }
    }
}
