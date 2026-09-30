package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.observe.DeckLane
import ai.magicbeans.magdroid.observe.ObserveDeckState
import ai.magicbeans.magdroid.observe.ambientSummary
import ai.magicbeans.magdroid.observe.catchUpSummary
import ai.magicbeans.magdroid.voice.AmbientPowerBlock
import ai.magicbeans.magdroid.voice.AmbientPowerMonitor
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ScreenShare
import androidx.compose.material.icons.outlined.BatteryStd
import androidx.compose.material.icons.outlined.CalendarMonth
import androidx.compose.material.icons.outlined.Mail
import androidx.compose.material.icons.outlined.Mic
import androidx.compose.material.icons.outlined.Notifications
import androidx.compose.material.icons.outlined.RssFeed
import androidx.compose.material.icons.outlined.Tab
import androidx.compose.material.icons.outlined.Update
import androidx.compose.material3.Icon
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** What the "This phone" group needs to render; all read by the caller. */
internal data class ThisPhoneState(
    val mic: PermissionStatus,
    val notifications: PermissionStatus,
    val notificationsApplicable: Boolean,
    val observeScreen: Boolean,
    val battery: AmbientPowerMonitor.Readings,
    val liveWarning: AmbientPowerBlock?,
)

/** EDITABLE: the device capture settings. */
@Composable
internal fun ThisPhoneSources(
    state: ThisPhoneState,
    onRequestMic: () -> Unit,
    onRequestNotifications: () -> Unit,
    onOpenAppSettings: () -> Unit,
    onOpenNotificationSettings: () -> Unit,
    onObserveScreenChange: (Boolean) -> Unit,
) {
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        DeckSectionHeader("This phone", hint = "Capture settings on this device")
        ObserveCard {
            PermissionRow(
                icon = Icons.Outlined.Mic,
                title = "Microphone",
                purpose = "Needed to listen to a room.",
                status = state.mic,
                onRequest = onRequestMic,
                onOpenSettings = onOpenAppSettings,
            )
            DeckDivider()
            PermissionRow(
                icon = Icons.Outlined.Notifications,
                title = "Notifications",
                purpose = "Observe runs as a foreground service; its notification carries Stop and Stop sharing. " +
                    "Without it, stop from the live bar or here.",
                status = state.notifications,
                onRequest = if (state.notificationsApplicable) onRequestNotifications else onOpenNotificationSettings,
                onOpenSettings = onOpenNotificationSettings,
            )
            DeckDivider()
            Row(verticalAlignment = Alignment.CenterVertically) {
                RowIcon(Icons.AutoMirrored.Outlined.ScreenShare, activePalette.discovery)
                Spacer(Modifier.width(10.dp))
                Column(Modifier.weight(1f)) {
                    Text("Also capture the screen", color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                    Text(
                        "Sends screen keyframes into the meeting thread beside the audio. Off unless you turn it on. " +
                            "Also in Settings.",
                        color = Muted, fontSize = 11.sp, lineHeight = 15.sp,
                    )
                }
                Switch(
                    checked = state.observeScreen,
                    onCheckedChange = onObserveScreenChange,
                    colors = SwitchDefaults.colors(
                        checkedThumbColor = OnAccent,
                        checkedTrackColor = Coral,
                        uncheckedTrackColor = Ground,
                        uncheckedBorderColor = BorderSoft,
                    ),
                )
            }
            DeckDivider()
            BatteryGuardRow(state.battery, state.liveWarning)
        }
    }
}

@Composable
private fun RowIcon(icon: ImageVector, tint: Color) = DeckIconBadge(icon, tint, size = 28, iconSize = 16)

@Composable
private fun PermissionRow(
    icon: ImageVector,
    title: String,
    purpose: String,
    status: PermissionStatus,
    onRequest: () -> Unit,
    onOpenSettings: () -> Unit,
) {
    val granted = status == PermissionStatus.Granted
    Row(verticalAlignment = Alignment.Top) {
        RowIcon(icon, if (granted) activePalette.success else MWarn)
        Spacer(Modifier.width(10.dp))
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(3.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(title, color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
                DeckPill(status.label.substringBefore(" —").uppercase(), if (granted) activePalette.success else MWarn)
            }
            Text(purpose, color = Muted, fontSize = 11.sp, lineHeight = 15.sp)
            when (status) {
                PermissionStatus.Granted -> Unit
                PermissionStatus.Blocked -> CompactAction(
                    "Open system settings", icon, onClick = onOpenSettings, modifier = Modifier.fillMaxWidth(),
                )
                else -> CompactAction("Allow $title".lowercase().replaceFirstChar(Char::uppercase), icon, onClick = onRequest, modifier = Modifier.fillMaxWidth())
            }
        }
    }
}

@Composable
private fun BatteryGuardRow(readings: AmbientPowerMonitor.Readings, liveWarning: AmbientPowerBlock?) {
    val floor = (AmbientPowerMonitor.BATTERY_FLOOR * 100).toInt()
    val admission = AmbientPowerMonitor.admit(readings)
    val refusal = admission.refusal
    val warning = liveWarning ?: admission.warning
    val tone = when {
        refusal != null -> Danger
        warning != null -> MWarn
        else -> activePalette.success
    }
    Row(verticalAlignment = Alignment.Top) {
        RowIcon(Icons.Outlined.BatteryStd, tone)
        Spacer(Modifier.width(10.dp))
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(3.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("Battery guard", color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
                val level = readings.batteryLevel.takeIf { it >= 0f }?.let { "${(it * 100).toInt()}%" } ?: "—"
                DeckPill(
                    (level + if (readings.isCharging) " · CHARGING" else ""),
                    tone,
                )
            }
            Text(
                "Listening won't start below $floor% battery unless the phone is charging. Once running it keeps " +
                    "going and warns instead — Battery Saver also shows a warning.",
                color = Muted, fontSize = 11.sp, lineHeight = 15.sp,
            )
            when {
                refusal != null -> Text(refusal.message, color = Danger, fontSize = 11.sp)
                warning != null -> Text(
                    if (liveWarning != null) "Live capture: ${liveWarning.shortLabel}" else warning.shortLabel,
                    color = MWarn, fontSize = 11.sp, fontWeight = FontWeight.SemiBold,
                )
                else -> Text("OK to capture now.", color = activePalette.success, fontSize = 11.sp)
            }
        }
    }
}

/** VIEW-ONLY: what the web and connected accounts observe. */
@Composable
internal fun WebAccountSources(state: ObserveDeckState, onRetry: (SourceBlock) -> Unit) {
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        DeckSectionHeader("Web & accounts", hint = "View-only here — change these on the web")

        SourceCard(SourceBlock.Channels, Icons.Outlined.Mail, activePalette.info, state.channels, onRetry) { channels ->
            if (channels.isEmpty()) {
                EmptyLine("No mail or chat accounts configured.")
            } else {
                channels.forEach { channel ->
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Column(Modifier.weight(1f)) {
                            Text(
                                "${channel.providerLabel} · ${channel.accountLabel}", color = Ink, fontSize = 12.sp,
                                fontWeight = FontWeight.Medium, maxLines = 1, overflow = TextOverflow.Ellipsis,
                            )
                            Text(
                                listOfNotNull(
                                    if (channel.connected) "connected" else "not connected",
                                    "${channel.threadCount} synced",
                                    channel.lane.takeIf(String::isNotBlank)?.let { "lane: ${it.replace('_', ' ')}" },
                                ).joinToString(" · "),
                                color = Muted, fontSize = 10.5.sp, maxLines = 1, overflow = TextOverflow.Ellipsis,
                            )
                        }
                        if (channel.hasVerificationCodes) {
                            DeckPill("CODES", activePalette.discovery)
                            Spacer(Modifier.width(4.dp))
                        }
                        OnOff(channel.enabled)
                    }
                }
            }
        }

        SourceCard(SourceBlock.Calendar, Icons.Outlined.CalendarMonth, activePalette.success, state.calendar, onRetry) { cal ->
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    if (cal.enabled) cal.scheduleLabel else "Not observing your calendar",
                    color = Ink, fontSize = 12.sp, modifier = Modifier.weight(1f),
                )
                OnOff(cal.enabled)
            }
            if (cal.accounts.isNotEmpty()) {
                Text(cal.accounts.joinToString(", "), color = Muted, fontSize = 11.sp, maxLines = 2, overflow = TextOverflow.Ellipsis)
            }
            Text(
                "Last sync: ${cal.lastSyncAt?.takeIf(String::isNotBlank) ?: "never"} · ${cal.totalSynced} synced",
                color = Muted, fontSize = 10.5.sp,
            )
        }

        SourceCard(SourceBlock.Subscriptions, Icons.Outlined.RssFeed, activePalette.warning, state.subscriptions, onRetry) { page ->
            if (page.items.isEmpty()) {
                EmptyLine("No continuous sources are listening.")
            } else {
                page.items.forEach { sub ->
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Column(Modifier.weight(1f)) {
                            Text(
                                sub.displayName.ifBlank { sub.sourceId }, color = Ink, fontSize = 12.sp,
                                fontWeight = FontWeight.Medium, maxLines = 1, overflow = TextOverflow.Ellipsis,
                            )
                            Text(
                                listOfNotNull(
                                    sub.providerLabel.takeIf(String::isNotBlank),
                                    sub.lastSuccessAtMs?.let { "last ${relativeWhen(it)}" },
                                    sub.nextRunAtMs?.let { "next ${nextWhen(it)}" },
                                ).joinToString(" · "),
                                color = Muted, fontSize = 10.5.sp, maxLines = 1, overflow = TextOverflow.Ellipsis,
                            )
                        }
                        DeckPill(
                            sub.stateLabel.uppercase(),
                            when (sub.stateLabel) {
                                "Listening" -> activePalette.success
                                "Retrying" -> MWarn
                                else -> Muted
                            },
                        )
                    }
                }
                if (page.total > page.items.size) {
                    Text("+${page.total - page.items.size} more on the web", color = Muted, fontSize = 10.5.sp)
                }
            }
        }

        SourceCard(SourceBlock.BrowserTabs, Icons.Outlined.Tab, activePalette.discovery, state.ambient, onRetry) { ambient ->
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(ambientSummary(ambient), color = Ink, fontSize = 12.sp, modifier = Modifier.weight(1f))
                OnOff(ambient.enabled)
            }
        }

        SourceCard(SourceBlock.CatchUp, Icons.Outlined.Update, Coral, state.catchUp, onRetry) { status ->
            Text(catchUpSummary(status), color = Ink, fontSize = 12.sp)
        }
    }
}

/** The five view-only blocks, each loaded and retried on its own. */
internal enum class SourceBlock(val title: String) {
    Channels("Mail & chat channels"),
    Calendar("Calendar observation"),
    Subscriptions("Continuous sources"),
    BrowserTabs("Browser tabs"),
    CatchUp("Startup catch-up"),
}

@Composable
private fun <T> SourceCard(
    block: SourceBlock,
    icon: ImageVector,
    tint: Color,
    lane: DeckLane<T>,
    onRetry: (SourceBlock) -> Unit,
    content: @Composable ColumnScope.(T) -> Unit,
) {
    ObserveCard {
        Row(verticalAlignment = Alignment.CenterVertically) {
            RowIcon(icon, tint)
            Spacer(Modifier.width(10.dp))
            Column(Modifier.weight(1f)) {
                Text(block.title, color = Ink, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                Text("Manage on web", color = Muted, fontSize = 10.sp)
            }
            if (lane.loading) {
                androidx.compose.material3.CircularProgressIndicator(
                    color = Coral, strokeWidth = 2.dp, modifier = Modifier.size(14.dp),
                )
            }
        }
        lane.error?.let { DeckErrorRow(it) { onRetry(block) } }
        val value = lane.value
        when {
            value != null -> content(value)
            lane.loading -> DeckLoadingRow("Loading…")
            lane.error == null -> EmptyLine("Not loaded yet.")
        }
    }
}

@Composable
private fun OnOff(on: Boolean) = DeckPill(if (on) "ON" else "OFF", if (on) activePalette.success else Muted)

@Composable
private fun EmptyLine(text: String) = Text(text, color = Muted, fontSize = 12.sp)

private fun nextWhen(epochMillis: Long, now: Long = System.currentTimeMillis()): String {
    val minutes = (epochMillis - now) / 60_000
    return when {
        minutes <= 0 -> "due"
        minutes < 60 -> "in $minutes min"
        minutes < 24 * 60 -> "in ${minutes / 60} h"
        else -> "in ${minutes / (24 * 60)} d"
    }
}
