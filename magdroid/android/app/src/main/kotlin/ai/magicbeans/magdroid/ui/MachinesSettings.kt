package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.access.MachinePickerRow
import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.access.RemoteMachineRegistry
import ai.magicbeans.magdroid.access.RemoteMachinesClient
import ai.magicbeans.magdroid.access.machinePickerRows
import ai.magicbeans.magdroid.push.AndroidMobilePushRegistration
import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.CancellationException

/**
 * Settings → Connection → Machines: every Magician this phone has paired, the
 * one in use, and the owner's other machines that are not paired yet.
 *
 * Tapping a paired machine switches to it without re-pairing; its own stored
 * credential is used. An unpaired machine cannot be switched to from here: it
 * offers only its web UI, where that machine mints its own owner-issued QR.
 * [onActiveChanged] rebinds screens that hold state from the old machine.
 */
@Composable
internal fun MachinesSection(onActiveChanged: () -> Unit) {
    val context = androidx.compose.ui.platform.LocalContext.current
    // Bumped after every switch or forget so the paired list re-reads.
    var revision by remember { mutableIntStateOf(0) }
    var registry by remember { mutableStateOf<RemoteMachineRegistry?>(null) }
    var registryProblem by remember { mutableStateOf<String?>(null) }
    var message by remember { mutableStateOf<String?>(null) }
    var forgetting by remember { mutableStateOf<MachinePickerRow?>(null) }

    val paired = remember(revision) { MagicianAccess.listProfiles(context) }
    val activeOrigin = paired.firstOrNull { it.active }?.origin
    // The registry comes from the active machine with that machine's
    // credential, so re-read it whenever the active machine changes.
    LaunchedEffect(activeOrigin) {
        if (activeOrigin == null) {
            registry = null
            return@LaunchedEffect
        }
        val client = RemoteMachinesClient(context)
        try {
            registry = client.fetch()
            registryProblem = null
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (error: Throwable) {
            registryProblem = error.message ?: "Could not list your other machines."
        } finally {
            client.close()
        }
    }
    val rows = machinePickerRows(paired, registry)
    if (rows.isEmpty()) return

    fun afterSwitch() {
        revision++
        // Push routes belong to the active machine: register with it.
        AndroidMobilePushRegistration.onActiveMachineChanged(context)
        onActiveChanged()
    }

    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text("Machines", color = Secondary, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
        Surface(color = Control, shape = RoundedCornerShape(8.dp), border = BorderStroke(1.dp, BorderSoft)) {
            Column {
                rows.forEachIndexed { index, row ->
                    if (index > 0) HorizontalDivider(color = BorderSoft, modifier = Modifier.padding(start = 12.dp))
                    MachineRow(
                        row = row,
                        onSwitch = {
                            message = try {
                                MagicianAccess.activate(context, row.origin)
                                afterSwitch()
                                "Now using ${row.title}."
                            } catch (error: IllegalArgumentException) {
                                error.message
                            } catch (error: IllegalStateException) {
                                error.message ?: "Could not switch machines."
                            }
                        },
                        onForget = { forgetting = row },
                        onOpenWeb = { openMachineWeb(context, row) },
                    )
                }
            }
        }
        Text(
            "Tap a paired machine to use it; each keeps its own pairing. To add a machine, open its web UI, then Settings → Mobile devices, and scan its QR here.",
            color = Muted, fontSize = 11.sp, lineHeight = 15.sp,
        )
        registryProblem?.let { Text(it, color = Muted, fontSize = 11.sp, lineHeight = 15.sp) }
        message?.let { Text(it, color = Secondary, fontSize = 11.sp, lineHeight = 15.sp) }
    }

    forgetting?.let { row ->
        AlertDialog(
            onDismissRequest = { forgetting = null },
            title = { Text("Forget ${row.title}?") },
            text = {
                Text(
                    "This phone deletes its pairing with ${row.title}. Using it again needs a new connection QR from that machine." +
                        if (row.active) " Another paired machine becomes active, if there is one." else "",
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    forgetting = null
                    message = try {
                        // Its coding_runs route goes first, while this phone
                        // still holds the credential that can remove it.
                        AndroidMobilePushRegistration.retireBeforeForget(context, row.origin)
                        MagicianAccess.forget(context, row.origin)
                        if (row.active) afterSwitch() else revision++
                        "Forgot ${row.title}."
                    } catch (error: IllegalStateException) {
                        error.message ?: "Could not forget that machine."
                    }
                }) { Text("Forget", color = Danger) }
            },
            dismissButton = {
                TextButton(onClick = { forgetting = null }) { Text("Cancel", color = Secondary) }
            },
        )
    }
}

@Composable
private fun MachineRow(
    row: MachinePickerRow,
    onSwitch: () -> Unit,
    onForget: () -> Unit,
    onOpenWeb: () -> Unit,
) {
    val switchable = row.paired && !row.active
    Row(
        Modifier
            .fillMaxWidth()
            .background(if (row.active) Coral.copy(alpha = 0.08f) else androidx.compose.ui.graphics.Color.Transparent)
            .clickable(enabled = switchable || !row.paired) { if (row.paired) onSwitch() else onOpenWeb() }
            .padding(horizontal = 12.dp, vertical = 10.dp),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(
                row.title,
                color = Ink, fontSize = 14.sp,
                fontWeight = if (row.active) FontWeight.SemiBold else FontWeight.Normal,
                maxLines = 1, overflow = TextOverflow.Ellipsis,
            )
            Text(
                when {
                    row.active -> "In use · ${row.origin.substringAfter("://")}"
                    row.paired -> "Paired · tap to use"
                    else -> "Pair: open its web UI → Settings → Mobile devices, then scan the QR"
                },
                color = Muted, fontSize = 11.sp, lineHeight = 15.sp,
                maxLines = 2, overflow = TextOverflow.Ellipsis,
            )
        }
        when {
            row.active -> Icon(
                Icons.Filled.CheckCircle, contentDescription = "In use",
                tint = Coral, modifier = Modifier.size(17.dp),
            )
            !row.paired -> TextButton(onClick = onOpenWeb) { Text("Pair", color = Coral, fontSize = 12.sp) }
            else -> Unit
        }
        if (row.paired) {
            TextButton(onClick = onForget) { Text("Forget", color = Danger, fontSize = 12.sp) }
        }
    }
}

/** Open the machine's own web UI; its Settings → Mobile devices shows the pairing QR. */
private fun openMachineWeb(context: Context, row: MachinePickerRow) {
    val target = row.uiOrigin ?: return
    try {
        context.startActivity(
            Intent(Intent.ACTION_VIEW, Uri.parse(target)).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
        )
    } catch (_: ActivityNotFoundException) {
        // No browser: the hint text already names where to go.
    }
}
