package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.tasks.LiveElapsed
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import kotlinx.coroutines.delay

/**
 * A running step's elapsed time (`12s`, `3m 4s`), ticking once a second.
 *
 * The clock lives in this composable, so it runs only while a running row is
 * on screen, and only while the screen is started: backgrounding the app or
 * scrolling the row away stops it, and a settled row (which shows its
 * duration instead) drops it.
 */
@Composable
internal fun LiveElapsedText(
    startedAtMillis: Long,
    style: TextStyle,
    fontFamily: FontFamily? = null,
    modifier: Modifier = Modifier,
) {
    val now = rememberLiveNowMillis(startedAtMillis)
    LiveElapsed.text(startedAtMillis, now)?.let {
        Text(it, style = style, fontFamily = fontFamily, maxLines = 1, modifier = modifier)
    }
}

/**
 * The wall clock, re-read every [tickMillis] (default [LiveElapsed.TICK_MILLIS])
 * while the screen is started. The one ticker behind every live elapsed label:
 * [LiveElapsedText] and the "Preparing final result" row both read it, so a
 * second timer is never needed; the Steps card's "Updated 2 min. ago" reads it
 * at a slower beat. [key] restarts the clock when the thing being timed changes.
 */
@Composable
internal fun rememberLiveNowMillis(key: Any?, tickMillis: Long = LiveElapsed.TICK_MILLIS): Long {
    var now by remember { mutableStateOf(System.currentTimeMillis()) }
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    LaunchedEffect(key, lifecycle, tickMillis) {
        lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
            while (true) {
                now = System.currentTimeMillis()
                delay(tickMillis)
            }
        }
    }
    return now
}
