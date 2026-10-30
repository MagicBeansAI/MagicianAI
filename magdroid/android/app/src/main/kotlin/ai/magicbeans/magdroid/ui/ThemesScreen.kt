package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.identity.ProductIdentity
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle

/**
 * Themes, opened from the side menu (iOS `ThemesView`): the System / Day /
 * Night switch and every theme family with a palette preview. Picking a family
 * applies it at once and keeps the sheet open, so themes can be compared in
 * place — the whole app repaints underneath.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun ThemesSheet(onDismiss: () -> Unit) {
    val context = LocalContext.current
    val store = remember(context) { ThemeStore.get(context) }
    val systemIsDark = isSystemInDarkTheme()
    // Read from the store, never a local copy: the activity watches the same
    // values and applies them.
    val familyId by store.familyId.collectAsStateWithLifecycle()
    val mode by store.mode.collectAsStateWithLifecycle()
    val selectedFamily = Themes.families.firstOrNull {
        it.id == Themes.migratedId(familyId) || it.dark == Themes.migratedId(familyId)
    }

    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        containerColor = Ground,
        shape = RoundedCornerShape(topStart = Lively.cardRadius, topEnd = Lively.cardRadius),
    ) {
        Column(
            Modifier
                .fillMaxWidth()
                .themeWash()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp)
                .padding(bottom = 30.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    "Themes",
                    color = Ink,
                    fontSize = 19.sp,
                    fontWeight = FontWeight.Bold,
                    modifier = Modifier.weight(1f),
                )
                TextButton(onClick = onDismiss, shape = MagicanButtonShape) {
                    Text("Done", color = Coral, fontWeight = FontWeight.SemiBold)
                }
            }

            ThemesSectionHeader("Appearance")
            AppearanceModePicker(
                current = mode,
                onSelect = { entry ->
                    store.setMode(entry)
                    applyTheme(familyId, entry, systemIsDark)
                    // An explicit pick follows the account, so every surface
                    // (web, iPhone) converges on it.
                    ThemeSync.pushAsync(context, systemIsDark)
                },
            )
            ThemesFooter(
                "System follows your device's Day/Night. The ${ProductIdentity.productName} keyboard always follows the device appearance, regardless of this.",
            )

            ThemesSectionHeader("Theme")
            ThemesGroup {
                Themes.families.forEachIndexed { index, family ->
                    val isSelected = family == selectedFamily
                        Row(
                            Modifier
                                .fillMaxWidth()
                                .heightIn(min = Lively.tapMin)
                                .clickable(role = Role.Button) {
                                    store.setFamily(family.id)
                                    applyTheme(family.id, mode, systemIsDark)
                                    // An explicit pick follows the account, so
                                    // every surface (web, iPhone) converges on it.
                                    ThemeSync.pushAsync(context, systemIsDark)
                                }
                            .semantics {
                                contentDescription = family.name
                                selected = isSelected
                            }
                            .padding(horizontal = 12.dp, vertical = 8.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        ThemeSwatch(previewPalette(family, mode, systemIsDark))
                        Text(
                            family.name,
                            color = Ink,
                            fontSize = 15.sp,
                            fontWeight = if (isSelected) FontWeight.SemiBold else FontWeight.Normal,
                            modifier = Modifier.weight(1f).padding(start = 12.dp),
                        )
                        if (isSelected) {
                            Icon(
                                Icons.Filled.CheckCircle,
                                contentDescription = null,
                                tint = Coral,
                                modifier = Modifier.size(18.dp),
                            )
                        }
                    }
                    if (index != Themes.families.lastIndex) {
                        HorizontalDivider(color = BorderSoft, modifier = Modifier.padding(start = 70.dp))
                    }
                }
            }
            ThemesFooter(
                if (mode == AppearanceMode.System) {
                    "Previews follow your device's current appearance."
                } else {
                    "Previews show each theme's ${mode.label.lowercase()} palette."
                },
            )
        }
    }
}

@Composable
private fun ThemesSectionHeader(title: String) {
    Text(
        title,
        color = Secondary,
        fontSize = 12.sp,
        fontWeight = FontWeight.SemiBold,
        modifier = Modifier.padding(top = 8.dp, start = 4.dp),
    )
}

@Composable
private fun ThemesFooter(text: String) {
    Text(
        text,
        color = Muted,
        fontSize = 11.sp,
        lineHeight = 15.sp,
        modifier = Modifier.padding(horizontal = 4.dp),
    )
}

@Composable
private fun ThemesGroup(content: @Composable () -> Unit) {
    Surface(
        color = Control,
        shape = Lively.cardShape,
        border = BorderStroke(1.dp, BorderSoft),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column { content() }
    }
}

/**
 * A truthful miniature of a palette — its real background, accent and text —
 * drawn from the [Palette] itself so a preview cannot drift from its theme.
 */
@Composable
internal fun ThemeSwatch(palette: Palette, modifier: Modifier = Modifier) {
    Row(
        modifier
            .size(width = 46.dp, height = 28.dp)
            .background(palette.background, RoundedCornerShape(7.dp))
            .border(1.dp, palette.accent.copy(alpha = 0.55f), RoundedCornerShape(7.dp))
            .padding(horizontal = 6.dp),
        horizontalArrangement = Arrangement.spacedBy(4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(
            Modifier
                .size(width = 12.dp, height = 4.dp)
                .background(palette.accent, RoundedCornerShape(2.dp)),
        )
        Box(
            Modifier
                .size(width = 18.dp, height = 4.dp)
                .background(palette.text.copy(alpha = 0.7f), RoundedCornerShape(2.dp)),
        )
    }
}

/**
 * The variant that would actually be applied right now: a family holds a day
 * and a night palette, and previewing the wrong one promises paper and
 * delivers a dark terminal.
 */
internal fun previewPalette(
    family: ThemeFamily,
    mode: AppearanceMode,
    systemIsDark: Boolean,
): Palette = Themes.palette(Themes.resolve(family.id, mode, systemIsDark))

/** Visible height of a System / Day / Night segment, as iOS's segmented picker. */
internal const val APPEARANCE_SEGMENT_HEIGHT_DP = 32

/** System / Day / Night as one compact segmented control, as iOS. */
@Composable
internal fun AppearanceModePicker(
    current: AppearanceMode,
    onSelect: (AppearanceMode) -> Unit,
) {
    val shape = Lively.chipShape
    Row(
        Modifier
            .fillMaxWidth()
            .clip(shape)
            .background(Control)
            .border(1.dp, BorderSoft, shape)
            .selectableGroup(),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        AppearanceMode.entries.forEachIndexed { index, entry ->
            val chosen = current == entry
            Box(
                Modifier
                    .weight(1f)
                    .background(if (chosen) AccentFill else Color.Transparent)
                    .selectable(selected = chosen, role = Role.RadioButton, onClick = { onSelect(entry) })
                    .height(APPEARANCE_SEGMENT_HEIGHT_DP.dp),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    entry.label,
                    color = if (chosen) OnAccent else Secondary,
                    fontSize = 12.sp,
                    fontWeight = if (chosen) FontWeight.SemiBold else FontWeight.Normal,
                )
            }
            if (index != AppearanceMode.entries.lastIndex) {
                Box(Modifier.width(1.dp).height(18.dp).background(BorderSoft))
            }
        }
    }
}
