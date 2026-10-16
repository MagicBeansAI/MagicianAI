package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.observe.ObserveAudioSurface
import ai.magicbeans.magdroid.observe.ObserveDeckState
import ai.magicbeans.magdroid.observe.audioProfileChoices
import ai.magicbeans.magdroid.voice.audioProfileLabel
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.GraphicEq
import androidx.compose.material.icons.outlined.Groups
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.RadioButton
import androidx.compose.material3.RadioButtonDefaults
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * Meeting and listening transcription profiles, the web's
 * `SurfaceAudioProfileControl` as native pickers. Saving is on selection,
 * optimistic, rolled back with the reason on failure.
 */
@Composable
internal fun ObserveAudioView(
    state: ObserveDeckState,
    onSelect: (ObserveAudioSurface, String?) -> Unit,
    onRetry: () -> Unit,
) {
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(10.dp)) {
        DeckSectionHeader(
            "Audio profiles",
            hint = "Speech-to-text used for meetings and room listening, for this account",
            loading = state.audioSelection.loading || state.audioCatalog.loading,
            onRefresh = onRetry,
        )
        (state.audioSelection.error ?: state.audioCatalog.error)?.let { DeckErrorRow(it, onRetry) }
        state.audioSaveError?.let { ObserveBanner(it) }
        val selection = state.audioSelection.value
        val catalog = state.audioCatalog.value
        if (selection == null || catalog == null) {
            if (state.audioSelection.loading || state.audioCatalog.loading) DeckLoadingRow("Loading audio profiles…")
            return@Column
        }
        ObserveAudioSurface.entries.forEach { surface ->
            val choices = audioProfileChoices(catalog, surface)
            val selected = selection[surface.wire]
            val default = catalog.defaultAudioProfiles[surface.wire]
            val saving = state.audioSaving == surface.wire
            ObserveCard {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    DeckIconBadge(
                        if (surface == ObserveAudioSurface.Meeting) Icons.Outlined.Groups else Icons.Outlined.GraphicEq,
                        activePalette.discovery, size = 28, iconSize = 16,
                    )
                    Spacer(Modifier.width(10.dp))
                    Column(Modifier.weight(1f)) {
                        Text(surface.label, color = Ink, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
                        Text(surface.hint, color = Muted, fontSize = 11.sp, lineHeight = 14.sp)
                    }
                    if (saving) CircularProgressIndicator(color = Coral, strokeWidth = 2.dp, modifier = Modifier.size(16.dp))
                }
                Text(
                    "Using: " + (selected?.let(::audioProfileLabel)
                        ?: default?.let { "${audioProfileLabel(it)} (configured default)" }
                        ?: "configured default"),
                    color = Secondary, fontSize = 12.sp,
                )
                Column(Modifier.selectableGroup(), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    ProfileOption(
                        title = "Use configured default",
                        description = default?.let { audioProfileLabel(it) } ?: "Whatever the server is configured to use",
                        selected = selected == null,
                        enabled = !saving && state.audioSaving == null,
                        onClick = { onSelect(surface, null) },
                    )
                    choices.forEach { choice ->
                        ProfileOption(
                            title = choice.label,
                            description = choice.description,
                            selected = selected == choice.id,
                            enabled = !saving && state.audioSaving == null,
                            onClick = { onSelect(surface, choice.id) },
                        )
                    }
                    if (choices.isEmpty()) {
                        Text("No ${surface.label.lowercase()} profiles are configured on this host.", color = Muted, fontSize = 11.sp)
                    }
                }
            }
        }
        Text(
            "Per-stage provider overrides stay on the web. Choosing a profile here clears them for that surface, as on the web.",
            color = Muted, fontSize = 10.5.sp, lineHeight = 14.sp,
        )
    }
}

@Composable
private fun ProfileOption(
    title: String,
    description: String,
    selected: Boolean,
    enabled: Boolean,
    onClick: () -> Unit,
) {
    val shape = RoundedCornerShape(10.dp)
    Surface(
        color = if (selected) Coral.copy(alpha = 0.07f) else Ground,
        shape = shape,
        border = BorderStroke(1.dp, if (selected) Coral else BorderSoft),
        modifier = Modifier
            .fillMaxWidth()
            .clip(shape)
            .clickable(enabled = enabled && !selected, role = Role.RadioButton, onClick = onClick),
    ) {
        Row(Modifier.padding(horizontal = 6.dp, vertical = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            RadioButton(
                selected = selected, onClick = null, enabled = enabled,
                colors = RadioButtonDefaults.colors(selectedColor = Coral, unselectedColor = Muted),
            )
            Spacer(Modifier.width(4.dp))
            Column(Modifier.weight(1f).padding(vertical = 4.dp)) {
                Text(title, color = Ink, fontSize = 13.sp, fontWeight = FontWeight.Medium)
                Text(description, color = Muted, fontSize = 10.5.sp, lineHeight = 14.sp)
            }
        }
    }
}
