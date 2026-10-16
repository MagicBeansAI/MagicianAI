package ai.magicbeans.magdroid.ui.vibedev

import ai.magicbeans.magdroid.vibedev.VibeDevUndo
import androidx.compose.material3.SnackbarDuration
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.SnackbarResult
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.lifecycle.compose.collectAsStateWithLifecycle

/**
 * Stop, Discard and Keep none: a snackbar with Undo for the two-second
 * window, with a haptic as it opens. The window itself lives in
 * [VibeDevUndo]; when it closes (sent or undone) the snackbar goes too.
 */
@Composable
internal fun VibeDevUndoSnackbar(undo: VibeDevUndo, snackbar: SnackbarHostState) {
    val pending by undo.pending.collectAsStateWithLifecycle()
    val haptic = LocalHapticFeedback.current
    LaunchedEffect(pending?.id) {
        val shown = pending ?: return@LaunchedEffect
        haptic.performHapticFeedback(HapticFeedbackType.LongPress)
        snackbar.currentSnackbarData?.dismiss()
        val result = snackbar.showSnackbar(shown.message, actionLabel = "Undo", duration = SnackbarDuration.Indefinite)
        if (result == SnackbarResult.ActionPerformed && undo.pending.value?.id == shown.id) undo.undo()
    }
}
