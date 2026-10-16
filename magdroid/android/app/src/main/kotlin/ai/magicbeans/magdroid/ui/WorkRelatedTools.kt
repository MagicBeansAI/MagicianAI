package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.work.*
import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp

/** Browser custody keeps the established web login and specialist permissions. */
@Composable
internal fun WorkRelatedTools(origin: WorkWebContext, destinations: List<WorkSpecialist> = WorkSpecialist.entries, liveSurface: Boolean = false) {
    val context = LocalContext.current
    val connection = remember { MagicianAccess.connectionSnapshot(context) }
    var expanded by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    WorkLink("Related tools") { expanded = !expanded }
    if (expanded) Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text("Opens your connected web console. Sign in there if asked; Back to Work returns to this item.", color = Muted)
        error?.let { Text(it, color = Danger) }
        if (liveSurface) WorkLink("Live agent surface in web console") {
            if (connection == null || connection != MagicianAccess.connectionSnapshot(context)) error = "Connection changed. Reopen this work view."
            else origin.workURL(connection.baseUrl)?.let { url ->
                try { context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url))) }
                catch (_: Exception) { error = "No browser is available to open this tool." }
            } ?: run { error = "This connection cannot open the web console." }
        }
        destinations.forEach { destination ->
            TextButton(onClick = {
                if (connection == null || connection != MagicianAccess.connectionSnapshot(context)) { error = "Connection changed. Reopen this work view."; return@TextButton }
                val url = origin.url(connection.baseUrl, destination)
                if (url == null) { error = "This connection cannot open the web console."; return@TextButton }
                try { context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url))) }
                catch (_: Exception) { error = "No browser is available to open this tool." }
            }, modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp)) {
                Text(if (destination == WorkSpecialist.Apps && origin.appInstallation.isNotEmpty()) "Originating App" else destination.title, color = Coral, modifier = Modifier.fillMaxWidth())
            }
            if (destination !in listOf(WorkSpecialist.Analytics, WorkSpecialist.History, WorkSpecialist.Memory, WorkSpecialist.Events) && !(destination == WorkSpecialist.Apps && origin.appInstallation.isNotEmpty())) Text("Workspace-wide", color = Muted)
        }
    }
}
