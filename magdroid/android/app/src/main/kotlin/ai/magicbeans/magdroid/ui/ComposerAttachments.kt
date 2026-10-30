package ai.magicbeans.magdroid.ui

import android.Manifest
import android.content.pm.PackageManager
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.foundation.layout.Column
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.platform.LocalContext
import androidx.core.content.ContextCompat
import androidx.core.content.FileProvider
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.File

internal data class ComposerAttachmentActions(val files: () -> Unit, val photos: () -> Unit)

/** Chat and VibeDev share document, photo-library and full-resolution camera picks. */
@Composable
internal fun rememberComposerAttachments(onPicked: (String, String, ByteArray) -> Unit, onError: (String) -> Unit = {}): ComposerAttachmentActions {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val latestPicked by rememberUpdatedState(onPicked)
    var menu by remember { mutableStateOf(false) }
    var cameraPath by rememberSaveable { mutableStateOf<String?>(null) }
    val read: (Uri?) -> Unit = { uri -> if (uri != null) scope.launch {
        val picked = withContext(Dispatchers.IO) { runCatching { readPicked(context, uri) }.getOrNull() }
        if (picked != null) latestPicked(picked.first, picked.second, picked.third)
        else onError("Could not read this attachment. Choose a file smaller than 20 MB.")
    } }
    val document = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument(), read)
    val image = rememberLauncherForActivityResult(ActivityResultContracts.GetContent(), read)
    val camera = rememberLauncherForActivityResult(ActivityResultContracts.TakePicture()) { saved ->
        val file = cameraPath?.let(::File)
        if (saved && file != null) scope.launch {
            val bytes = withContext(Dispatchers.IO) { runCatching { if (file.length() <= 20 * 1024 * 1024) file.readBytes() else null }.getOrNull().also { file.delete() } }
            if (bytes != null) latestPicked(file.name, "image/jpeg", bytes) else onError("Could not read the photo.")
        } else file?.delete()
        cameraPath = null
    }
    val launchCamera = {
        val directory = File(context.cacheDir, "magician-artifacts").apply { mkdirs() }
        val file = File.createTempFile("camera-", ".jpg", directory)
        cameraPath = file.absolutePath
        val uri = FileProvider.getUriForFile(context, "${context.packageName}.artifact-files", file)
        camera.launch(uri)
    }
    val permission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { allowed ->
        if (allowed) launchCamera() else onError("Camera permission was not granted.")
    }
    if (menu) AlertDialog(onDismissRequest = { menu = false }, title = { Text("Add an attachment") },
        text = { Column {
            TextButton(onClick = { menu = false; document.launch(arrayOf("*/*")) }) { Text("Choose file") }
            TextButton(onClick = { menu = false; image.launch("image/*") }) { Text("Photo library") }
            TextButton(onClick = {
                menu = false
                if (ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) launchCamera()
                else permission.launch(Manifest.permission.CAMERA)
            }) { Text("Take photo") }
        } }, confirmButton = { TextButton(onClick = { menu = false }) { Text("Cancel") } },
        containerColor = activePalette.elevated, titleContentColor = activePalette.text, textContentColor = activePalette.text)
    return ComposerAttachmentActions(files = { menu = true }, photos = { menu = true })
}
