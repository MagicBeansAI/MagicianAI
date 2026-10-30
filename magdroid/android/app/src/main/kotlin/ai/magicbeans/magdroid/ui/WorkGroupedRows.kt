package ai.magicbeans.magdroid.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsFocusedAsState
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.KeyboardArrowLeft
import androidx.compose.material.icons.automirrored.outlined.KeyboardArrowRight
import androidx.compose.material.icons.automirrored.outlined.OpenInNew
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.Cancel
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.Inbox
import androidx.compose.material.icons.outlined.Info
import ai.magicbeans.magdroid.work.WorkHelpTopic
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material.icons.outlined.KeyboardArrowDown
import androidx.compose.material.icons.outlined.Remove
import androidx.compose.material.icons.outlined.UnfoldMore
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.*
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/*
 * The grouped-card design the crew screens share with iOS (`WorkGroupedRows`):
 * a titled card of rows. The title sits above the card, inset to the row
 * text; rows carry their own touch target and are separated by hairlines;
 * forms are rows too (label leading, control trailing). Colours come from the
 * active palette only, so every theme family, day and night, renders it.
 */

internal const val WORK_SCREEN_PADDING_DP = 16
internal const val WORK_GROUP_SPACING_DP = 20
internal const val WORK_CARD_RADIUS_DP = 14
internal const val WORK_CARD_PADDING_H_DP = 16
internal const val WORK_ROW_MIN_HEIGHT_DP = 48
internal const val WORK_ROW_PADDING_V_DP = 10

/** Help has its own action; opening it never triggers the surrounding control. */
@Composable
internal fun WorkHelpButton(topic: WorkHelpTopic, modifier: Modifier = Modifier, buttonSize: Dp = 48.dp) {
    var showing by remember(topic) { mutableStateOf(false) }
    Box(modifier) {
        IconButton(onClick = { showing = !showing }, modifier = Modifier.size(buttonSize)) {
            Icon(Icons.Outlined.Info, "About ${topic.title}", tint = Muted, modifier = Modifier.size(18.dp))
        }
        DropdownMenu(expanded = showing, onDismissRequest = { showing = false }, containerColor = activePalette.elevated) {
            Column(Modifier.widthIn(max = 300.dp).padding(horizontal = 16.dp, vertical = 8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(topic.title, color = Ink, fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
                Text(topic.explanation, color = Ink, fontSize = 14.sp, lineHeight = 20.sp)
                TextButton(onClick = { showing = false }, colors = ButtonDefaults.textButtonColors(contentColor = Coral)) { Text("Done") }
            }
        }
    }
}

/** At this text scale a trailing value or control moves under its label. */
private const val WORK_STACK_FONT_SCALE = 1.5f

@Composable
internal fun stackedForTextSize(): Boolean = LocalDensity.current.fontScale >= WORK_STACK_FONT_SCALE

/** What a row shows at its trailing edge. */
internal enum class WorkRowAccessory { None, Chevron, Collapsed, Expanded, Menu, External }

/**
 * A card title: uppercase on screen, announced in sentence case as a heading
 * (screen readers spell out capitals, and tests find the title by its text).
 */
@Composable
internal fun WorkGroupTitle(title: String, modifier: Modifier = Modifier) {
    Row(modifier.padding(horizontal = WORK_CARD_PADDING_H_DP.dp), verticalAlignment = Alignment.CenterVertically) {
    Text(
        title.uppercase(), color = Muted, fontSize = 12.sp, fontWeight = FontWeight.SemiBold, letterSpacing = 0.6.sp,
        modifier = Modifier.weight(1f, fill = false)
            .clearAndSetSemantics { heading(); text = AnnotatedString(title) },
    )
    WorkHelpTopic.forLabel(title)?.let { WorkHelpButton(it) }
    }
}

/** A titled card of rows: card fill, hairline border, 14 corners, 16 inner inset, no vertical inset. */
@Composable
internal fun WorkGroup(title: String? = null, modifier: Modifier = Modifier, content: @Composable ColumnScope.() -> Unit) {
    Column(modifier.fillMaxWidth()) {
        title?.let { WorkGroupTitle(it); Spacer(Modifier.height(8.dp)) }
        Surface(
            color = activePalette.card, shape = RoundedCornerShape(WORK_CARD_RADIUS_DP.dp),
            border = BorderStroke(1.dp, BorderSoft), modifier = Modifier.fillMaxWidth(),
        ) {
            Column(Modifier.fillMaxWidth().padding(horizontal = WORK_CARD_PADDING_H_DP.dp), content = content)
        }
    }
}

/** One device pixel in the card-border colour, spanning the card's inner width. */
@Composable
internal fun WorkRowDivider(modifier: Modifier = Modifier) {
    val hairline = with(LocalDensity.current) { 1f.toDp() }
    Box(modifier.fillMaxWidth().height(hairline).background(BorderSoft))
}

/** Labelled fields stacked inside a card: 10 apart, padded from the rows around them. */
@Composable
internal fun WorkFieldStack(modifier: Modifier = Modifier, content: @Composable ColumnScope.() -> Unit) {
    Column(modifier.fillMaxWidth().padding(vertical = 12.dp), verticalArrangement = Arrangement.spacedBy(10.dp), content = content)
}

/** A panel nested in a card (an expanded focus area): page fill, hairline border, 10 corners. */
@Composable
internal fun WorkInsetPanel(modifier: Modifier = Modifier, content: @Composable ColumnScope.() -> Unit) {
    Surface(color = Ground, shape = RoundedCornerShape(10.dp), border = BorderStroke(1.dp, BorderSoft), modifier = modifier.fillMaxWidth()) {
        Column(Modifier.fillMaxWidth().padding(horizontal = 12.dp), content = content)
    }
}

@Composable
internal fun WorkRowIcon(icon: ImageVector?, tint: Color) {
    if (icon == null) return
    // Sized in sp so icons grow with the text, as iOS symbols do.
    val (column, glyph) = with(LocalDensity.current) { 24.sp.toDp() to 16.sp.toDp() }
    Box(Modifier.width(column), contentAlignment = Alignment.Center) { Icon(icon, contentDescription = null, tint = tint, modifier = Modifier.size(glyph)) }
    Spacer(Modifier.width(12.dp))
}

@Composable
internal fun WorkAccessoryIcon(accessory: WorkRowAccessory, tint: Color = Muted.copy(alpha = 0.75f)) {
    val icon = when (accessory) {
        WorkRowAccessory.None -> return
        WorkRowAccessory.Chevron, WorkRowAccessory.Collapsed -> Icons.AutoMirrored.Outlined.KeyboardArrowRight
        WorkRowAccessory.Expanded -> Icons.Outlined.KeyboardArrowDown
        WorkRowAccessory.Menu -> Icons.Outlined.UnfoldMore
        WorkRowAccessory.External -> Icons.AutoMirrored.Outlined.OpenInNew
    }
    val glyph = with(LocalDensity.current) { (if (accessory == WorkRowAccessory.External) 16.sp else 20.sp).toDp() }
    Spacer(Modifier.width(8.dp))
    Icon(icon, contentDescription = null, tint = tint, modifier = Modifier.size(glyph))
}

/**
 * A row: optional accent icon, a wrapping label, an optional single-line
 * secondary value and a trailing accessory. The whole row is the touch target.
 */
@Composable
internal fun WorkRow(
    label: String,
    modifier: Modifier = Modifier,
    icon: ImageVector? = null,
    value: String? = null,
    detail: String? = null,
    accessory: WorkRowAccessory = WorkRowAccessory.Chevron,
    labelColor: Color = Ink,
    iconTint: Color = Coral,
    enabled: Boolean = true,
    onClickLabel: String? = null,
    /** A control at the trailing edge (a Restore button), outside the row's own touch target. */
    trailing: (@Composable () -> Unit)? = null,
    onClick: (() -> Unit)? = null,
) {
    val stacked = value != null && stackedForTextSize()
    val expandable = accessory == WorkRowAccessory.Collapsed || accessory == WorkRowAccessory.Expanded
    BoxWithConstraints(modifier.fillMaxWidth()) {
        val valueMax = maxWidth * 0.45f
        Row(
            Modifier.fillMaxWidth().heightIn(min = WORK_ROW_MIN_HEIGHT_DP.dp)
                .then(if (onClick != null) Modifier.clickable(enabled = enabled, role = Role.Button, onClickLabel = onClickLabel, onClick = onClick) else Modifier)
                .then(if (expandable) Modifier.semantics { stateDescription = if (accessory == WorkRowAccessory.Expanded) "Expanded" else "Collapsed" } else Modifier)
                .alpha(if (enabled) 1f else 0.45f)
                .padding(vertical = WORK_ROW_PADDING_V_DP.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            WorkRowIcon(icon, iconTint)
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Text(label, color = labelColor, fontSize = 16.sp)
                if (stacked) Text(value!!, color = Muted, fontSize = 15.sp)
                detail?.let { Text(it, color = Muted, fontSize = 13.sp, lineHeight = 17.sp) }
            }
            if (value != null && !stacked) {
                Spacer(Modifier.width(8.dp))
                Text(value, color = Muted, fontSize = 15.sp, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.widthIn(max = valueMax))
            }
            trailing?.let { Spacer(Modifier.width(8.dp)); it() }
            WorkAccessoryIcon(accessory)
        }
    }
}

/** An action (add, edit, run, merge) in the accent, or a removal in the danger colour; no chevron. */
@Composable
internal fun WorkActionRow(label: String, icon: ImageVector?, modifier: Modifier = Modifier, destructive: Boolean = false, enabled: Boolean = true, onClick: () -> Unit) {
    val color = if (destructive) Danger else Coral
    WorkRow(label, modifier, icon = icon, accessory = WorkRowAccessory.None, labelColor = color, iconTint = color, enabled = enabled, onClick = onClick)
}

/** A switch as a row: the label leads, the accent-filled switch trails, the whole row toggles. */
@Composable
internal fun WorkSwitchRow(label: String, checked: Boolean, onCheckedChange: (Boolean) -> Unit, modifier: Modifier = Modifier, enabled: Boolean = true) {
    Row(
        modifier.fillMaxWidth().heightIn(min = WORK_ROW_MIN_HEIGHT_DP.dp)
            .toggleable(value = checked, enabled = enabled, role = Role.Switch, onValueChange = onCheckedChange)
            .padding(vertical = WORK_ROW_PADDING_V_DP.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(label, color = Ink, fontSize = 16.sp, modifier = Modifier.weight(1f))
        WorkHelpTopic.forLabel(label)?.let { WorkHelpButton(it) }
        Spacer(Modifier.width(12.dp))
        Switch(checked = checked, onCheckedChange = null, enabled = enabled, colors = magicanSwitchColors())
    }
}

/** A counter: label, the value in secondary semibold, then a −/+ pair (each a 48dp target). */
@Composable
internal fun WorkStepperRow(label: String, value: Int, range: IntRange, onChange: (Int) -> Unit, modifier: Modifier = Modifier, display: String? = null) {
    val stepper: @Composable () -> Unit = {
        Box(contentAlignment = Alignment.Center) {
            Surface(color = Control, shape = RoundedCornerShape(8.dp), border = BorderStroke(1.dp, ControlBorder), modifier = Modifier.matchParentSize().padding(vertical = 6.dp)) {}
            Row(verticalAlignment = Alignment.CenterVertically) {
                StepperSegment(Icons.Outlined.Remove, "Decrease $label", value > range.first) { onChange(value - 1) }
                Box(Modifier.width(1.dp).height(20.dp).background(ControlBorder))
                StepperSegment(Icons.Outlined.Add, "Increase $label", value < range.last) { onChange(value + 1) }
            }
        }
    }
    val number: @Composable () -> Unit = { Text(display ?: "$value", color = Muted, fontSize = 16.sp, fontWeight = FontWeight.SemiBold) }
    if (stackedForTextSize()) Column(modifier.fillMaxWidth().padding(vertical = WORK_ROW_PADDING_V_DP.dp)) {
        Text(label, color = Ink, fontSize = 16.sp)
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) { number(); Spacer(Modifier.weight(1f)); stepper() }
    } else Row(modifier.fillMaxWidth().heightIn(min = WORK_ROW_MIN_HEIGHT_DP.dp).padding(vertical = 4.dp), verticalAlignment = Alignment.CenterVertically) {
        Text(label, color = Ink, fontSize = 16.sp, modifier = Modifier.weight(1f))
        Spacer(Modifier.width(12.dp)); number(); Spacer(Modifier.width(12.dp)); stepper()
    }
}

@Composable
private fun StepperSegment(icon: ImageVector, description: String, enabled: Boolean, onClick: () -> Unit) {
    Box(
        Modifier.size(48.dp).clip(RoundedCornerShape(8.dp))
            .clickable(enabled = enabled, role = Role.Button, onClick = onClick)
            .semantics { contentDescription = description },
        contentAlignment = Alignment.Center,
    ) { Icon(icon, contentDescription = null, tint = if (enabled) Ink else Muted.copy(alpha = 0.45f), modifier = Modifier.size(with(LocalDensity.current) { 18.sp.toDp() })) }
}

/**
 * A one-of-many choice as a row: the label leads and the current choice trails
 * in the accent with an up-down mark; tapping opens a menu anchored to the row.
 * Stored values are untouched; [options] pairs each value with its label.
 */
@Composable
internal fun WorkChoiceRow(
    label: String,
    options: List<Pair<String, String>>,
    selected: String?,
    onSelect: (String) -> Unit,
    modifier: Modifier = Modifier,
    icon: ImageVector? = null,
    optionTag: ((String) -> String)? = null,
    enabled: Boolean = true,
    /** Replaces what screen readers (and tests) read for the row, e.g. "Crew member: fixture-crew". */
    semanticLabel: String? = null,
) {
    var open by remember { mutableStateOf(false) }
    val current = options.firstOrNull { it.first.equals(selected, ignoreCase = true) }?.second
        ?: selected?.takeIf(String::isNotBlank)?.let(::workLabel) ?: "Choose"
    Box(modifier.fillMaxWidth()) {
        WorkPickerRow(label, current, icon = icon, enabled = enabled, semanticLabel = semanticLabel) { open = true }
        // A zero-size anchor at the trailing edge, so the menu opens under the choice.
        Box(Modifier.align(Alignment.BottomEnd)) {
            DropdownMenu(open, { open = false }, containerColor = activePalette.elevated) {
                val check: @Composable () -> Unit = { Icon(Icons.Outlined.Check, contentDescription = "Selected", tint = Coral, modifier = Modifier.size(18.dp)) }
                options.forEach { (value, text) ->
                    val isSelected = value.equals(selected, ignoreCase = true)
                    DropdownMenuItem(
                        text = { Text(text, color = if (isSelected) Coral else Ink, fontWeight = if (isSelected) FontWeight.SemiBold else FontWeight.Normal) },
                        onClick = { open = false; onSelect(value) },
                        trailingIcon = if (isSelected) check else null,
                        modifier = Modifier.heightIn(min = WORK_ROW_MIN_HEIGHT_DP.dp).then(optionTag?.let { Modifier.testTag(it(value)) } ?: Modifier),
                    )
                }
            }
        }
    }
}

/**
 * The face of a picker: label leading, the current choice trailing in the
 * accent with an up-down mark. At large text the choice wraps under the label.
 */
@Composable
internal fun WorkPickerRow(label: String, current: String, modifier: Modifier = Modifier, icon: ImageVector? = null, enabled: Boolean = true, semanticLabel: String? = null, onClick: () -> Unit) {
    val measurer = rememberTextMeasurer()
    val style = LocalTextStyle.current.copy(fontSize = 16.sp)
    val density = LocalDensity.current
    val large = stackedForTextSize()
    BoxWithConstraints(modifier.fillMaxWidth()) {
        // Stack the choice under the label when both do not fit on one line.
        val needed = with(density) {
            val text = measurer.measure(label, style, maxLines = 1).size.width + measurer.measure(current, style, maxLines = 1).size.width
            text.toDp() + (if (icon != null) 36.sp.toDp() else 0.dp) + 12.dp + 28.sp.toDp()
        }
        val stacked = large || needed > maxWidth
        val choice: @Composable () -> Unit = {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(current, color = Coral, fontSize = 16.sp, maxLines = 3, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f, fill = false))
                WorkAccessoryIcon(WorkRowAccessory.Menu, tint = Coral)
            }
        }
        val row = Modifier.fillMaxWidth().heightIn(min = WORK_ROW_MIN_HEIGHT_DP.dp)
            .clickable(enabled = enabled, role = Role.Button, onClickLabel = "Change $label", onClick = onClick)
            .then(if (semanticLabel != null) Modifier.clearAndSetSemantics {
                text = AnnotatedString(semanticLabel); role = Role.Button
                if (enabled) onClick(label = "Change $label") { onClick(); true } else disabled()
            } else Modifier.semantics { stateDescription = current })
            .alpha(if (enabled) 1f else 0.45f)
            .padding(vertical = WORK_ROW_PADDING_V_DP.dp)
        if (stacked) Column(row, verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) { WorkRowIcon(icon, Coral); Text(label, color = Ink, fontSize = 16.sp) }
            choice()
        } else Row(row, verticalAlignment = Alignment.CenterVertically) {
            WorkRowIcon(icon, Coral)
            Text(label, color = Ink, fontSize = 16.sp, modifier = Modifier.weight(1f))
            Spacer(Modifier.width(12.dp))
            choice()
        }
    }
}

/**
 * A labelled field: caption above (13 semibold, secondary), then the field —
 * control fill, hairline border (accent when focused), 8 corners, 12/10 inset,
 * at least 48 tall, 15sp text.
 */
@Composable
internal fun WorkLabeledField(
    caption: String,
    value: String,
    onValueChange: (String) -> Unit,
    modifier: Modifier = Modifier,
    fieldModifier: Modifier = Modifier,
    enabled: Boolean = true,
    minLines: Int = 1,
    maxLines: Int = 2,
    mono: Boolean = false,
    /** A numeric keyboard for amounts (`KeyboardType.Decimal`, `Number`). */
    keyboardType: androidx.compose.ui.text.input.KeyboardType? = null,
) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    val shape = RoundedCornerShape(8.dp)
    Column(modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        // The field carries the caption for screen readers and tests (as a
        // text field's label does); the visible caption is not read twice.
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(caption, color = Muted, fontSize = 13.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f, fill = false).clearAndSetSemantics {})
            WorkHelpTopic.forLabel(caption)?.let { WorkHelpButton(it) }
        }
        BasicTextField(
            value = value, onValueChange = onValueChange, enabled = enabled,
            minLines = minLines.coerceAtMost(maxLines), maxLines = maxLines,
            textStyle = LocalTextStyle.current.copy(color = if (enabled) Ink else Muted, fontSize = if (mono) 14.sp else 15.sp,
                fontFamily = if (mono) LocalMagicanFontFamilies.current.mono else LocalTextStyle.current.fontFamily),
            keyboardOptions = when {
                keyboardType != null -> androidx.compose.foundation.text.KeyboardOptions(keyboardType = keyboardType, autoCorrectEnabled = false)
                mono -> androidx.compose.foundation.text.KeyboardOptions(capitalization = androidx.compose.ui.text.input.KeyboardCapitalization.None, autoCorrectEnabled = false)
                else -> androidx.compose.foundation.text.KeyboardOptions.Default
            },
            cursorBrush = SolidColor(Coral), interactionSource = interaction,
            modifier = fieldModifier.fillMaxWidth().semantics { text = AnnotatedString(caption) },
            decorationBox = { inner ->
                Box(
                    Modifier.fillMaxWidth().heightIn(min = 48.dp).background(Control, shape)
                        .border(if (focused) 1.5.dp else 1.dp, if (focused) Coral else ControlBorder, shape)
                        .padding(horizontal = 12.dp, vertical = 10.dp),
                    contentAlignment = if (minLines > 1) Alignment.TopStart else Alignment.CenterStart,
                ) { inner() }
            },
        )
    }
}

/** The Work chips' shape: a 6dp rounded square, as the web's chips. */
internal val WorkChipShape = RoundedCornerShape(6.dp)

/** A compact status label: 12 semibold, 6dp corners, the colour at 12% fill and 35% border. */
@Composable
internal fun WorkChip(text: String, color: Color, icon: ImageVector? = null) {
    Row(
        Modifier.clip(WorkChipShape).background(color.copy(alpha = 0.12f)).border(1.dp, color.copy(alpha = 0.35f), WorkChipShape)
            .padding(horizontal = 10.dp, vertical = 4.dp),
        horizontalArrangement = Arrangement.spacedBy(4.dp), verticalAlignment = Alignment.CenterVertically,
    ) {
        icon?.let { Icon(it, contentDescription = null, tint = color, modifier = Modifier.size(with(LocalDensity.current) { 12.sp.toDp() })) }
        Text(text, color = color, fontSize = 12.sp, fontWeight = FontWeight.SemiBold, maxLines = 1, softWrap = false, overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis, modifier = Modifier.weight(1f, fill = false))
    }
}

/** A crew status colour: grey when disabled, otherwise the shared [workStatusTone]. */
internal fun workStatusColor(status: String, disabled: Boolean = false): Color = if (disabled) Muted else workStatusTone(status)

/** A status chip's tone for a runtime status word, shared by every Work list. */
internal fun workStatusTone(status: String): Color = when (status.lowercase()) {
    "active", "running", "triggered", "accepted", "complete", "completed", "captured", "healthy", "approved", "succeeded" -> activePalette.success
    "error", "failed", "partial", "rejected", "revoked", "cancelled", "budget_exhausted", "needs_attention" -> Danger
    "paused", "pending", "review", "awaiting_review", "waiting_owner", "attention", "plateau", "drafting", "proposal" -> MWarn
    else -> Muted
}

/** One chip of a row's chip line: text, tone and an optional leading icon. */
internal data class WorkChipSpec(val text: String, val color: Color, val icon: ImageVector? = null)

/** Chips that wrap onto further lines rather than overflow the row. */
@Composable
@OptIn(ExperimentalLayoutApi::class)
internal fun WorkChipRow(chips: List<WorkChipSpec>, modifier: Modifier = Modifier) {
    if (chips.isEmpty()) return
    FlowRow(modifier, horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        chips.forEach { WorkChip(it.text, it.color, it.icon) }
    }
}

/**
 * A list entry: icon, semibold title, secondary lines and chips, chevron
 * trailing; the whole entry is one touch target. Lists put these in a
 * [WorkGroup] separated by [WorkRowDivider].
 */
@Composable
internal fun WorkItemRow(
    title: String,
    modifier: Modifier = Modifier,
    subtitle: String? = null,
    detail: String? = null,
    icon: ImageVector? = null,
    iconTint: Color = Coral,
    chips: List<WorkChipSpec> = emptyList(),
    accessory: WorkRowAccessory = WorkRowAccessory.Chevron,
    trailing: (@Composable () -> Unit)? = null,
    enabled: Boolean = true,
    onClick: (() -> Unit)? = null,
) {
    Row(
        modifier.fillMaxWidth().heightIn(min = WORK_ROW_MIN_HEIGHT_DP.dp)
            .then(if (onClick != null) Modifier.clickable(enabled = enabled, role = Role.Button, onClick = onClick) else Modifier)
            .alpha(if (enabled) 1f else 0.45f)
            .padding(vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (icon != null) Row(Modifier.align(Alignment.Top).padding(top = 1.dp)) { WorkRowIcon(icon, iconTint) }
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(title, color = Ink, fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
            subtitle?.takeIf(String::isNotBlank)?.let { Text(it, color = Muted, fontSize = 13.sp, lineHeight = 17.sp) }
            detail?.takeIf(String::isNotBlank)?.let { Text(it, color = Ink.copy(alpha = 0.85f), fontSize = 14.sp, lineHeight = 19.sp, maxLines = 3, overflow = TextOverflow.Ellipsis) }
            WorkChipRow(chips, Modifier.padding(top = 2.dp))
        }
        trailing?.let { Spacer(Modifier.width(8.dp)); it() }
        WorkAccessoryIcon(accessory)
    }
}

/**
 * The summary card at the top of a detail screen: title, secondary lines and
 * chips, then optional [rows] (actions) under a hairline.
 */
@Composable
internal fun WorkHeaderCard(
    title: String,
    modifier: Modifier = Modifier,
    subtitle: String? = null,
    detail: String? = null,
    chips: List<WorkChipSpec> = emptyList(),
    trailing: (@Composable () -> Unit)? = null,
    rows: (@Composable ColumnScope.() -> Unit)? = null,
) {
    WorkGroup(modifier = modifier) {
        Row(Modifier.fillMaxWidth().padding(vertical = WORK_CARD_PADDING_H_DP.dp), verticalAlignment = Alignment.Top) {
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(title, color = Ink, fontSize = 22.sp, fontWeight = FontWeight.SemiBold, lineHeight = 27.sp, modifier = Modifier.semantics { heading() })
                subtitle?.takeIf(String::isNotBlank)?.let { SelectionContainerText(it, Muted, 13.sp) }
                detail?.takeIf(String::isNotBlank)?.let { Text(it, color = Ink, fontSize = 15.sp, lineHeight = 20.sp) }
                WorkChipRow(chips, Modifier.padding(top = 2.dp))
            }
            trailing?.let { Spacer(Modifier.width(8.dp)); it() }
        }
        rows?.let { WorkRowDivider(); it() }
    }
}

@Composable
private fun SelectionContainerText(text: String, color: Color, size: androidx.compose.ui.unit.TextUnit) {
    androidx.compose.foundation.text.selection.SelectionContainer { Text(text, color = color, fontSize = size, lineHeight = size * 1.3f) }
}

/** Secondary explanatory text inside a card, between rows. */
@Composable
internal fun WorkNote(text: String, modifier: Modifier = Modifier, color: Color = Muted) {
    Text(text, color = color, fontSize = 13.sp, lineHeight = 17.sp, modifier = modifier.fillMaxWidth().padding(vertical = 10.dp))
}

/** A footnote under a card, inset to the row text like a group title. */
@Composable
internal fun WorkFootnote(text: String, modifier: Modifier = Modifier, color: Color = Muted) {
    Text(text, color = color, fontSize = 12.sp, lineHeight = 16.sp, modifier = modifier.fillMaxWidth().padding(horizontal = WORK_CARD_PADDING_H_DP.dp))
}

/** Previous · Page n · Next for server-paged lists; arrows only at large text. */
@Composable
internal fun WorkPager(page: Int, hasMore: Boolean, onPage: (Int) -> Unit, modifier: Modifier = Modifier, disabled: Boolean = false) {
    val compact = stackedForTextSize()
    val side: @Composable (String, Boolean, Boolean, () -> Unit) -> Unit = { label, enabled, leading, onClick ->
        Row(
            Modifier.heightIn(min = WORK_ROW_MIN_HEIGHT_DP.dp).widthIn(min = WORK_ROW_MIN_HEIGHT_DP.dp).clip(RoundedCornerShape(8.dp))
                .clickable(enabled = enabled, role = Role.Button, onClickLabel = label, onClick = onClick)
                .semantics { contentDescription = label }
                .alpha(if (enabled) 1f else 0.4f).padding(horizontal = 4.dp),
            verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            val glyph = with(LocalDensity.current) { 18.sp.toDp() }
            if (leading) Icon(Icons.AutoMirrored.Outlined.KeyboardArrowLeft, null, tint = Coral, modifier = Modifier.size(glyph))
            if (!compact) Text(label, color = Coral, fontSize = 15.sp, fontWeight = FontWeight.Medium, modifier = Modifier.clearAndSetSemantics {})
            if (!leading) Icon(Icons.AutoMirrored.Outlined.KeyboardArrowRight, null, tint = Coral, modifier = Modifier.size(glyph))
        }
    }
    Row(modifier.fillMaxWidth().padding(horizontal = 8.dp), verticalAlignment = Alignment.CenterVertically) {
        side("Previous", page > 0 && !disabled, true) { onPage((page - 1).coerceAtLeast(0)) }
        Text("Page ${page + 1}", color = Muted, fontSize = 13.sp, fontWeight = FontWeight.SemiBold, textAlign = TextAlign.Center, modifier = Modifier.weight(1f))
        side("Next", hasMore && !disabled, false) { onPage(page + 1) }
    }
}

/**
 * The in-page search box: control fill, hairline border, 12 corners, a
 * leading magnifier and a clear button. The prompt is the field's text while
 * it is empty, so tests and screen readers find the field by it.
 */
@Composable
internal fun WorkSearchField(prompt: String, value: String, onValueChange: (String) -> Unit, modifier: Modifier = Modifier, enabled: Boolean = true) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    val shape = RoundedCornerShape(12.dp)
    BasicTextField(
        value = value, onValueChange = onValueChange, enabled = enabled, singleLine = true,
        textStyle = LocalTextStyle.current.copy(color = Ink, fontSize = 16.sp),
        cursorBrush = SolidColor(Coral), interactionSource = interaction,
        modifier = modifier.fillMaxWidth().semantics { text = AnnotatedString(prompt) },
        decorationBox = { inner ->
            Row(
                Modifier.fillMaxWidth().heightIn(min = WORK_ROW_MIN_HEIGHT_DP.dp).background(Control, shape)
                    .border(if (focused) 1.5.dp else 1.dp, if (focused) Coral else ControlBorder, shape)
                    .padding(start = 12.dp, end = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Icon(Icons.Outlined.Search, null, tint = Muted, modifier = Modifier.size(with(LocalDensity.current) { 18.sp.toDp() }))
                Spacer(Modifier.width(8.dp))
                Box(Modifier.weight(1f).padding(vertical = 10.dp)) {
                    if (value.isEmpty()) Text(prompt, color = Muted.copy(alpha = 0.8f), fontSize = 16.sp, maxLines = 2, modifier = Modifier.clearAndSetSemantics {})
                    inner()
                }
                if (value.isNotEmpty() && enabled) Box(
                    Modifier.size(WORK_ROW_MIN_HEIGHT_DP.dp).clip(CircleShape).clickable(role = Role.Button, onClickLabel = "Clear search") { onValueChange("") }
                        .semantics { contentDescription = "Clear search" },
                    contentAlignment = Alignment.Center,
                ) { Icon(Icons.Outlined.Cancel, null, tint = Muted.copy(alpha = 0.7f), modifier = Modifier.size(18.dp)) }
                else Spacer(Modifier.width(8.dp))
            }
        },
    )
}

/** A centred empty, error or loading message for a whole list. */
@Composable
internal fun WorkEmptyState(title: String, modifier: Modifier = Modifier, icon: ImageVector = Icons.Outlined.Inbox, message: String? = null) {
    Column(
        modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 28.dp).semantics(mergeDescendants = true) {},
        horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Icon(icon, null, tint = Muted.copy(alpha = 0.6f), modifier = Modifier.size(with(LocalDensity.current) { 28.sp.toDp() }))
        Text(title, color = Ink, fontSize = 16.sp, fontWeight = FontWeight.SemiBold, textAlign = TextAlign.Center)
        message?.let { Text(it, color = Muted, fontSize = 14.sp, lineHeight = 19.sp, textAlign = TextAlign.Center) }
    }
}

/**
 * Two or three mutually exclusive views on one track (Inbox · Runs,
 * Guided · Advanced JSON): the selected segment is a raised card.
 */
@Composable
internal fun WorkSegmented(options: List<Pair<String, String>>, selected: String, onSelect: (String) -> Unit, modifier: Modifier = Modifier) {
    Row(
        modifier.fillMaxWidth().clip(RoundedCornerShape(12.dp)).background(Control).border(1.dp, ControlBorder, RoundedCornerShape(12.dp)).padding(3.dp),
        horizontalArrangement = Arrangement.spacedBy(3.dp),
    ) {
        options.forEach { (value, label) ->
            val active = value == selected
            Box(
                Modifier.weight(1f).heightIn(min = 42.dp).clip(RoundedCornerShape(9.dp))
                    .background(if (active) activePalette.card else Color.Transparent)
                    .then(if (active) Modifier.border(1.dp, BorderSoft, RoundedCornerShape(9.dp)) else Modifier)
                    .selectable(selected = active, role = Role.Tab) { onSelect(value) }
                    .padding(horizontal = 6.dp, vertical = 8.dp),
                contentAlignment = Alignment.Center,
            ) {
                Text(label, color = if (active) Ink else Muted, fontSize = 15.sp, fontWeight = if (active) FontWeight.SemiBold else FontWeight.Medium, textAlign = TextAlign.Center, maxLines = 1, softWrap = false, overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis)
            }
        }
    }
}

/**
 * The bar over a pushed Work screen: Back leading, the screen title centred,
 * then Refresh and New work. Dialogs that stand in for a screen use it with
 * their own back label and actions.
 */
@Composable
internal fun WorkDetailBar(
    title: String,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
    backLabel: String = "‹ Back",
    actions: @Composable RowScope.() -> Unit = {},
) {
    Row(
        modifier.fillMaxWidth().background(Ground).heightIn(min = TASK_HEADER_HEIGHT_DP.dp).padding(horizontal = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        TextButton(onClick = onBack, modifier = Modifier.heightIn(min = 48.dp), contentPadding = PaddingValues(horizontal = 12.dp)) {
            Text(backLabel, color = Coral, fontSize = 16.sp, maxLines = 1)
        }
        Text(
            title, color = Ink, fontSize = TASK_HEADER_TITLE_FONT_SP.sp, fontWeight = FontWeight.SemiBold, textAlign = TextAlign.Center,
            maxLines = 2, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f).padding(horizontal = 4.dp).semantics { heading() },
        )
        actions()
    }
}

/** A 48dp icon action for a Work bar. */
@Composable
internal fun WorkBarIcon(icon: ImageVector, description: String, enabled: Boolean = true, tint: Color = Coral, onClick: () -> Unit) {
    IconButton(onClick = onClick, enabled = enabled, modifier = Modifier.size(48.dp)) { Icon(icon, description, tint = if (enabled) tint else Muted) }
}
