package ai.magicbeans.magdroid.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsFocusedAsState
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
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.LocalTextStyle
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ProvideTextStyle
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

internal const val COMPACT_INPUT_HEIGHT_DP = 40
internal const val COMPACT_LABELED_INPUT_HEIGHT_DP = 42
internal const val COMPACT_INPUT_LINE_STEP_DP = 16
internal const val COMPACT_INPUT_FONT_SP = 13

/**
 * The shared Android form field.
 *
 * Material's stock outlined field reserves a 56dp desktop-like container even
 * for one short value. iOS uses a much denser rounded field throughout Magican, so
 * every Android form uses this primitive instead of independently forcing a
 * stock field shorter (which clips or vertically misaligns its text).
 *
 * Labels stay present as a small overline after a value is entered. Placeholder
 * and input occupy the same line, and multiline fields grow by actual text
 * lines rather than inheriting the single-line Material minimum repeatedly.
 */
@Composable
internal fun MagicianTextField(
    value: String,
    onValueChange: (String) -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    readOnly: Boolean = false,
    textStyle: TextStyle = LocalTextStyle.current.copy(color = Ink, fontSize = COMPACT_INPUT_FONT_SP.sp),
    label: (@Composable () -> Unit)? = null,
    placeholder: (@Composable () -> Unit)? = null,
    leadingIcon: (@Composable () -> Unit)? = null,
    trailingIcon: (@Composable () -> Unit)? = null,
    supportingText: (@Composable () -> Unit)? = null,
    visualTransformation: VisualTransformation = VisualTransformation.None,
    keyboardOptions: KeyboardOptions = KeyboardOptions.Default,
    keyboardActions: KeyboardActions = KeyboardActions.Default,
    singleLine: Boolean = false,
    maxLines: Int = if (singleLine) 1 else Int.MAX_VALUE,
    minLines: Int = 1,
) {
    val interactionSource = remember { MutableInteractionSource() }
    val focused by interactionSource.collectIsFocusedAsState()
    val resolvedMaxLines = if (singleLine) 1 else maxLines.coerceAtLeast(1)
    val resolvedMinLines = if (singleLine) 1 else minLines.coerceIn(1, resolvedMaxLines)
    // An empty label-only field behaves like iOS's placeholder. Reserving a
    // second blank value row here is what made Android forms look twice as tall
    // as their iOS counterparts. Once content exists (or a distinct placeholder
    // is present), the label becomes the persistent compact overline.
    val stackedLabel = label != null && (value.isNotEmpty() || placeholder != null)
    val baseHeight = if (stackedLabel) COMPACT_LABELED_INPUT_HEIGHT_DP else COMPACT_INPUT_HEIGHT_DP
    val minimumHeight = (baseHeight + (resolvedMinLines - 1) * COMPACT_INPUT_LINE_STEP_DP).dp
    val foreground = if (enabled) Ink else Muted.copy(alpha = 0.58f)
    val border = if (focused) Coral else ControlBorder

    Column(modifier = modifier, verticalArrangement = Arrangement.spacedBy(3.dp)) {
        Surface(
            modifier = Modifier.fillMaxWidth(),
            color = Control,
            shape = RoundedCornerShape(10.dp),
            border = BorderStroke(if (focused) 1.5.dp else 1.dp, border),
        ) {
            BasicTextField(
                value = value,
                onValueChange = onValueChange,
                enabled = enabled,
                readOnly = readOnly,
                singleLine = singleLine,
                minLines = resolvedMinLines,
                maxLines = resolvedMaxLines,
                textStyle = textStyle.copy(color = foreground),
                visualTransformation = visualTransformation,
                keyboardOptions = keyboardOptions,
                keyboardActions = keyboardActions,
                interactionSource = interactionSource,
                cursorBrush = SolidColor(Coral),
                modifier = Modifier.fillMaxWidth().heightIn(min = minimumHeight),
                decorationBox = { field ->
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .heightIn(min = minimumHeight)
                            .padding(horizontal = 11.dp, vertical = if (singleLine) 0.dp else 7.dp),
                        verticalAlignment = if (singleLine) Alignment.CenterVertically else Alignment.Top,
                    ) {
                        leadingIcon?.let { icon ->
                            CompositionLocalProvider(LocalContentColor provides if (focused) Coral else Muted) {
                                Box(Modifier.size(24.dp), contentAlignment = Alignment.Center) { icon() }
                            }
                            Box(Modifier.width(7.dp))
                        }
                        Column(
                            modifier = Modifier.weight(1f),
                            verticalArrangement = if (singleLine) Arrangement.Center else Arrangement.Top,
                        ) {
                            label?.takeIf { stackedLabel }?.let { content ->
                                CompositionLocalProvider(LocalContentColor provides if (focused) Coral else Muted) {
                                    ProvideTextStyle(MaterialTheme.typography.labelSmall.copy(fontSize = 9.sp)) {
                                        Box(Modifier.height(12.dp), contentAlignment = Alignment.TopStart) { content() }
                                    }
                                }
                            }
                            Box(
                                modifier = Modifier.fillMaxWidth().then(
                                    if (singleLine && stackedLabel) Modifier.height(22.dp) else Modifier,
                                ),
                                contentAlignment = if (singleLine) Alignment.CenterStart else Alignment.TopStart,
                            ) {
                                if (value.isEmpty()) {
                                    CompositionLocalProvider(LocalContentColor provides Muted) {
                                        ProvideTextStyle(MaterialTheme.typography.bodyMedium.copy(fontSize = 12.sp)) {
                                            if (placeholder != null) placeholder()
                                            else label?.invoke()
                                        }
                                    }
                                }
                                field()
                            }
                        }
                        trailingIcon?.let { icon ->
                            Box(Modifier.width(7.dp))
                            CompositionLocalProvider(LocalContentColor provides Muted) {
                                Box(Modifier.size(28.dp), contentAlignment = Alignment.Center) { icon() }
                            }
                        }
                    }
                },
            )
        }
        supportingText?.let { content ->
            CompositionLocalProvider(LocalContentColor provides Muted) {
                ProvideTextStyle(MaterialTheme.typography.bodySmall.copy(fontSize = 10.sp, lineHeight = 13.sp)) {
                    Box(Modifier.padding(horizontal = 3.dp)) { content() }
                }
            }
        }
    }
}
