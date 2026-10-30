package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.work.*
import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Link
import androidx.compose.material.icons.outlined.Sensors
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * Browser custody keeps the established web login and specialist permissions.
 *
 * Rendered as grouped rows: an expandable "Related tools" row, then one
 * external-link row per destination. It sits inside a [WorkGroup] or a
 * [WorkCard]; neither adds horizontal inset to the rows.
 */
@Composable
internal fun WorkRelatedTools(origin: WorkWebContext, destinations: List<WorkSpecialist> = WorkSpecialist.entries, liveSurface: Boolean = false) {
    val context = LocalContext.current
    val connection = remember { MagicianAccess.connectionSnapshot(context) }
    var expanded by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    val open: (String?) -> Unit = { url ->
        if (url == null) error = "This connection cannot open the web console."
        else try { context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url))) }
        catch (_: Exception) { error = "No browser is available to open this tool." }
    }
    val connected: () -> Boolean = {
        (connection != null && connection == MagicianAccess.connectionSnapshot(context)).also { if (!it) error = "Connection changed. Reopen this work view." }
    }
    WorkRow("Related tools", icon = Icons.Outlined.Link, accessory = if (expanded) WorkRowAccessory.Expanded else WorkRowAccessory.Collapsed) { expanded = !expanded }
    if (expanded) Column(Modifier.fillMaxWidth().padding(bottom = 4.dp)) {
        Text("Opens your connected web console. Sign in there if asked; Back to Work returns to this item.", color = Muted, fontSize = 13.sp, lineHeight = 17.sp, modifier = Modifier.padding(bottom = 8.dp))
        error?.let { Text(it, color = Danger, fontSize = 13.sp, modifier = Modifier.padding(bottom = 8.dp)) }
        if (liveSurface) {
            WorkRowDivider()
            WorkRow("Live agent surface in web console", icon = Icons.Outlined.Sensors, accessory = WorkRowAccessory.External) {
                if (connected()) open(origin.workURL(connection!!.baseUrl))
            }
        }
        destinations.forEach { destination ->
            val originatingApp = destination == WorkSpecialist.Apps && origin.appInstallation.isNotEmpty()
            val workspaceWide = destination !in listOf(WorkSpecialist.Analytics, WorkSpecialist.History, WorkSpecialist.Memory, WorkSpecialist.Events) && !originatingApp
            WorkRowDivider()
            WorkRow(if (originatingApp) "Originating App" else destination.title, value = if (workspaceWide) "Workspace-wide" else null, accessory = WorkRowAccessory.External) {
                if (connected()) open(origin.url(connection!!.baseUrl, destination))
            }
        }
    }
}
