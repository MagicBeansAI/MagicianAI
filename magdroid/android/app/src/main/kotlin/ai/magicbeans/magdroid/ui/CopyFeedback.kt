package ai.magicbeans.magdroid.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.os.Build
import android.view.HapticFeedbackConstants
import android.widget.Toast
import androidx.compose.animation.Crossfade
import androidx.compose.animation.core.tween
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.heightIn
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.delay

/**
 * Tap-to-copy with feedback, shared by every surface that shows a shortened
 * id or a file name (web: a click on the elided id copies the full one and
 * the chip ticks for a moment).
 *
 * A copy does four things: puts the full value on the clipboard (label
 * `Magican <what>`), fires a light haptic, flips the tapped element to
 * `Copied ✓` for [CopyFeedbackRules.FLASH_MILLIS], and — only below API 33,
 * where the system shows no clipboard confirmation of its own — a short
 * `Copied <what>` toast.
 */
object CopyFeedbackRules {
    /** How long the tapped element reads `Copied ✓`. */
    const val FLASH_MILLIS = 1_200L

    /** What the tapped element reads while the flash holds. */
    const val COPIED_LABEL = "Copied ✓"

    /** What a screen reader hears when the flash appears. */
    const val ANNOUNCEMENT = "Copied"

    const val CLICK_LABEL_ID = "Copy full id"
    const val CLICK_LABEL_PATH = "Copy path"

    /** The ClipData label (shown by some keyboards' clipboard history). */
    fun clipLabel(what: String): String = "Magican ${what.trim().ifEmpty { "text" }}"

    /** The fallback toast below API 33. */
    fun toastText(what: String): String = "Copied ${what.trim().ifEmpty { "text" }}"

    /** API 33+ shows its own clipboard confirmation; a toast there doubles it. */
    fun showsToast(sdkInt: Int): Boolean = sdkInt < Build.VERSION_CODES.TIRAMISU

    /** The flash is a crossfade unless animations are off. */
    fun crossfades(animatorDurationScale: Float): Boolean = animatorDurationScale > 0f

    /** The label an element shows: its own text, or the flash. */
    fun label(display: String, copied: Boolean, copiedLabel: String = COPIED_LABEL): String =
        if (copied) copiedLabel else display
}

/**
 * What a tappable element copies, and how it is offered. An id element
 * shows a shortened id and copies the full one; a file element shows a
 * name and copies its absolute path, falling back to the relative one.
 */
data class CopyTarget(val value: String, val what: String, val clickLabel: String) {
    companion object {
        /** A shortened id: copies [fullId] trimmed. [what] is e.g. "task id". */
        fun id(fullId: String, what: String = "task id"): CopyTarget =
            CopyTarget(fullId.trim(), what, CopyFeedbackRules.CLICK_LABEL_ID)

        /** A file name: copies [absolutePath] when it is one, else [relativePath]. */
        fun path(absolutePath: String?, relativePath: String): CopyTarget =
            CopyTarget(
                absolutePath?.trim()?.takeIf { it.startsWith("/") } ?: relativePath.trim(),
                "path",
                CopyFeedbackRules.CLICK_LABEL_PATH,
            )
    }
}

/**
 * Clipboard + haptic + (API < 33) toast, with no per-element flash (use
 * [rememberCopyFlash] / [CopyableText] for that). `what` names the value in
 * the toast and the ClipData label ("task id", "path", "run id"). A blank
 * value is not copied.
 */
@Composable
fun rememberCopyAction(): (text: String, what: String) -> Unit {
    val copy = rememberClipboardCopy()
    return remember(copy) { { text: String, what: String -> copy(text, what); Unit } }
}

/** [rememberCopyAction], answering whether the clipboard took the value. */
@Composable
internal fun rememberClipboardCopy(): (text: String, what: String) -> Boolean {
    val context = LocalContext.current
    val view = LocalView.current
    return remember(context, view) {
        { text: String, what: String ->
            val copied = text.isNotBlank() && writeClipboard(context, CopyFeedbackRules.clipLabel(what), text)
            if (copied) {
                view.performHapticFeedback(
                    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) HapticFeedbackConstants.CONFIRM
                    else HapticFeedbackConstants.VIRTUAL_KEY,
                )
                if (CopyFeedbackRules.showsToast(Build.VERSION.SDK_INT)) {
                    Toast.makeText(context, CopyFeedbackRules.toastText(what), Toast.LENGTH_SHORT).show()
                }
            }
            copied
        }
    }
}

/** One element's copy + its `Copied ✓` flash. Hold one per tappable element. */
@Stable
class CopyFlash internal constructor(private val action: (String, String) -> Boolean) {
    internal var generation by mutableIntStateOf(0)
    internal var flashing by mutableIntStateOf(0)

    /** True while the element should read `Copied ✓`. */
    val copied: Boolean get() = flashing != 0

    /** Copies [target]'s value; on success the element flashes. */
    fun copy(target: CopyTarget) = copy(target.value, target.what)

    /** Copies [text]; on success the element flashes. */
    fun copy(text: String, what: String) {
        if (action(text, what)) {
            generation += 1
            flashing = generation
        }
    }
}

/** A [CopyFlash] whose flash clears itself after [CopyFeedbackRules.FLASH_MILLIS]. */
@Composable
fun rememberCopyFlash(): CopyFlash {
    val action = rememberClipboardCopy()
    val flash = remember(action) { CopyFlash(action) }
    val current = flash.flashing
    LaunchedEffect(flash, current) {
        if (current != 0) {
            delay(CopyFeedbackRules.FLASH_MILLIS)
            // A second tap restarts the clock; only the latest flash clears it.
            if (flash.flashing == current) flash.flashing = 0
        }
    }
    return flash
}

/**
 * [display] or `Copied ✓`, crossfaded (a plain swap when animations are
 * off). A polite live region, so TalkBack says "Copied" when it flips.
 */
@Composable
fun CopyFlashLabel(
    display: String,
    copied: Boolean,
    style: TextStyle,
    modifier: Modifier = Modifier,
    copiedColor: Color = activePalette.success,
    maxLines: Int = 1,
    // An element that already swaps its icon to a check passes "Copied".
    copiedLabel: String = CopyFeedbackRules.COPIED_LABEL,
) {
    val body: @Composable (Boolean) -> Unit = { showCopied ->
        Text(
            CopyFeedbackRules.label(display, showCopied, copiedLabel),
            style = if (showCopied) style.copy(color = copiedColor) else style,
            maxLines = maxLines,
            overflow = TextOverflow.Ellipsis,
        )
    }
    val announced = modifier.semantics {
        liveRegion = LiveRegionMode.Polite
    }
    if (CopyFeedbackRules.crossfades(rememberAnimatorDurationScale())) {
        Crossfade(copied, announced, animationSpec = tween(160), label = "copy-flash") { body(it) }
    } else {
        androidx.compose.foundation.layout.Box(announced) { body(copied) }
    }
}

/**
 * A tappable text that copies [copyValue] (not what it shows) and flashes
 * `Copied ✓`. [clickLabel] is what TalkBack offers ("Copy full id" /
 * "Copy path"). Put it OUTSIDE any parent `clickable`, or the parent eats
 * the tap.
 */
@Composable
fun CopyableText(
    display: String,
    copyValue: String,
    what: String,
    style: TextStyle,
    modifier: Modifier = Modifier,
    clickLabel: String = CopyFeedbackRules.CLICK_LABEL_ID,
    maxLines: Int = 1,
) {
    val flash = rememberCopyFlash()
    CopyFlashLabel(
        display = display,
        copied = flash.copied,
        style = style,
        maxLines = maxLines,
        modifier = modifier
            .heightIn(min = 24.dp)
            .clickable(onClickLabel = clickLabel, role = Role.Button) { flash.copy(copyValue, what) },
    )
}

/** Puts [value] on the clipboard; false when the system refused. */
internal fun writeClipboard(context: Context, label: String, value: String): Boolean = runCatching {
    val manager = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
    manager.setPrimaryClip(ClipData.newPlainText(label, value))
    true
}.getOrDefault(false)
