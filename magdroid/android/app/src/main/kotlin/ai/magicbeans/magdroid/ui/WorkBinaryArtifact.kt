package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.work.*
import android.content.Intent
import androidx.compose.foundation.layout.*
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.core.content.FileProvider
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.File

/** Only a frozen scoped download is handed to a viewer; no bearer appears in a URI. */
@Composable
internal fun WorkBinaryArtifact(path: String, name: String, mime: String?, resource: WorkResource, state: WorkResourceState, onReviewed: (() -> Unit)? = null) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var file by remember(path, name) { mutableStateOf<File?>(null) }
    LaunchedEffect(state.identity) { file = null }
    WorkLink("Load exact file: $name") {
        val identity = state.identity
        scope.launch {
            val data = resource.file("$path/file?name=${WorkRepository.segment(name)}", identity) ?: return@launch
            val saved = try {
                withContext(Dispatchers.IO) {
                    val folder = File(context.cacheDir, "magician-artifacts").apply { mkdirs() }
                    val safeName = name.substringAfterLast('/').replace(Regex("[^A-Za-z0-9._ -]"), "_").take(120).ifBlank { "artifact" }
                    File(folder, "${java.util.UUID.randomUUID()}-$safeName").apply { writeBytes(data) }
                }
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (_: Exception) {
                if (identity == resource.state.value.identity) resource.reportError("Could not save the artifact for preview. Free some space and try again.")
                return@launch
            }
            if (identity == resource.state.value.identity) file = saved
        }
    }
    file?.let { saved ->
        WorkLink("Preview exact file") {
            try {
                val uri = FileProvider.getUriForFile(context, "${context.packageName}.artifact-files", saved)
                context.startActivity(Intent(Intent.ACTION_VIEW).apply { setDataAndType(uri, mime ?: "application/octet-stream"); addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION) })
            } catch (_: Exception) { resource.reportError("No viewer is installed for this file type. Save it from the file menu.") }
        }
        WorkLink("Save or share exact file") {
            try {
                val uri = FileProvider.getUriForFile(context, "${context.packageName}.artifact-files", saved)
                context.startActivity(Intent.createChooser(Intent(Intent.ACTION_SEND).apply { type = mime ?: "application/octet-stream"; putExtra(Intent.EXTRA_STREAM, uri); addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION) }, "Save exact file"))
            } catch (_: Exception) { resource.reportError("Could not open the file menu.") }
        }
        if (onReviewed != null) TextButton(onClick = onReviewed, modifier = Modifier.heightIn(min = 48.dp)) { Text("I reviewed this file", color = Coral) }
    }
}
