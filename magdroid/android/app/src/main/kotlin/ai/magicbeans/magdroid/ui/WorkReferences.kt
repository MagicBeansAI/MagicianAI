package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.work.*
import android.provider.OpenableColumns
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch
import kotlinx.serialization.json.*
import java.nio.ByteBuffer
import java.nio.charset.CodingErrorAction
import java.util.UUID

@Composable
internal fun WorkReferencePicker(references: List<WorkSourceReference>, onChanged: (List<WorkSourceReference>) -> Unit, onPending: (Boolean) -> Unit, disabled: Boolean) {
    val resource = rememberWorkResource("/api/magician/v2/notes/tree")
    val state by resource.state.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    var query by remember { mutableStateOf("") }
    var results by remember { mutableStateOf<JsonElement?>(null) }
    var pending by remember { mutableStateOf(false) }
    val currentReferences by rememberUpdatedState(references)
    val currentDisabled by rememberUpdatedState(disabled)
    LaunchedEffect(state.identity) { results = null; query = ""; pending = false; onPending(false) }
    val importer = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri != null && !currentDisabled) runCatching {
            val bytes = context.contentResolver.openInputStream(uri)?.use { input ->
                val output = java.io.ByteArrayOutputStream(); val buffer = ByteArray(8192)
                while (output.size() <= 1_048_576) { val read = input.read(buffer, 0, minOf(buffer.size, 1_048_577 - output.size())); if (read < 0) break; if (read > 0) output.write(buffer, 0, read) }
                output.toByteArray()
            } ?: error("Could not read the selected document.")
            require(bytes.size <= 1_048_576) { "Attach a UTF-8 text document up to 1 MB. Larger sources use the complete contract." }
            val content = Charsets.UTF_8.newDecoder().onMalformedInput(CodingErrorAction.REPORT).onUnmappableCharacter(CodingErrorAction.REPORT).decode(ByteBuffer.wrap(bytes)).toString()
            require(!content.contains('\u0000')) { "Attach a UTF-8 text document. Binary sources use the complete contract." }
            var title = uri.lastPathSegment ?: "Attached document"
            context.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor -> if (cursor.moveToFirst()) cursor.getString(0)?.let { title = it } }
            onChanged(currentReferences + WorkSourceReference("file:${UUID.randomUUID()}", title, "Uploaded file: $title", content))
        }.onFailure { resource.reportError(it.message ?: "Could not read this text document.") }
    }
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text("References · ${references.size} attached", color = Ink, fontSize = 16.sp)
        Text("Their contents are frozen when you review the contract.", color = Muted, fontSize = 12.sp)
        OutlinedTextField(query, { query = it; results = null }, label = { Text("Find a reference") }, enabled = !disabled && !pending, modifier = Modifier.fillMaxWidth())
        TextButton(onClick = {
            val provider = state.value.workText("provider") ?: return@TextButton
            val requested = query.trim(); val identity = state.identity
            pending = true; onPending(true)
            scope.launch {
                try { val response = resource.perform("/api/magician/v2/notes/search", identity, body = buildJsonObject { put("provider", provider); put("query", requested); put("limit", 12) }, successNotice = null)
                    if (response != null && query.trim() == requested && !currentDisabled) results = response
                } finally { pending = false; onPending(false) }
            }
        }, enabled = !disabled && !pending && query.isNotBlank() && !state.value.workText("provider").isNullOrBlank(), modifier = Modifier.heightIn(min = 48.dp)) { Text("Search notes", color = Coral) }
        TextButton(onClick = { importer.launch(arrayOf("*/*")) }, enabled = !disabled && !pending, modifier = Modifier.heightIn(min = 48.dp)) { Text("Attach text file", color = Coral) }
        WorkResourceStatus(state, resource)
        results?.let { found ->
            found.workRows("hits").forEach { hit ->
                TextButton(onClick = {
                    val path = hit.workText("relative_path") ?: return@TextButton; val id = hit.workText("source_ref") ?: return@TextButton
                    val identity = state.identity; pending = true; onPending(true)
                    scope.launch {
                        try {
                            val note = resource.perform("/api/magician/v2/notes/file?path=${WorkRepository.segment(path)}", identity, method = WorkMethod.Get, body = null)
                            if (note != null && !currentDisabled) {
                                if (note.workField("provider") != hit.workField("provider") || note.workText("relative_path") != path || note.workText("markdown") == null) resource.reportError("The notes provider changed. Search and attach this reference again.")
                                else { onChanged(currentReferences.filter { it.id != id } + WorkSourceReference(id, note.workText("title") ?: path, id, note.workText("markdown").orEmpty())); results = null; query = "" }
                            }
                        } finally { pending = false; onPending(false) }
                    }
                }, enabled = !disabled && !pending, modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp)) { Text(hit.workText("title") ?: hit.workText("relative_path").orEmpty(), color = Coral) }
                Text(hit.workText("relative_path").orEmpty(), color = Muted, fontSize = 11.sp)
            }
            if (found.workRows("hits").isEmpty()) Text("No matching references.", color = Muted, fontSize = 13.sp)
            if (found.workBool("more_available") || found.workBool("scan_truncated")) Text("More documents may be available. Narrow the search.", color = Muted, fontSize = 12.sp)
        }
        references.forEach { reference ->
            var expanded by remember(reference.id) { mutableStateOf(false) }
            WorkLink(reference.title) { expanded = !expanded }
            if (expanded) { Text(reference.origin, color = Muted, fontSize = 11.sp); SelectionContainer { Text(reference.content, color = Ink, fontSize = 13.sp) } }
            TextButton(onClick = { onChanged(currentReferences.filter { it.id != reference.id }) }, enabled = !disabled && !pending, modifier = Modifier.heightIn(min = 48.dp)) { Text("Remove ${reference.title}", color = Danger) }
        }
    }
}
