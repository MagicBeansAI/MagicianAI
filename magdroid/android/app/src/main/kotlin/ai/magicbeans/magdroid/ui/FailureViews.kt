package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.net.Failure
import ai.magicbeans.magdroid.net.FailureKind
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.CloudOff
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material.icons.automirrored.outlined.HelpOutline
import androidx.compose.material.icons.outlined.Lock
import androidx.compose.material.icons.outlined.WifiOff
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * The three ways a failure is shown, so that one host being down looks the same
 * everywhere.
 *
 * Which one to use is decided by what is already on screen, not by how bad the
 * failure is:
 *
 * - [FailurePane] when there is nothing to show. It replaces the content, so it
 *   must never be confused with an empty state — "Nothing needs you" and "I
 *   could not find out whether anything needs you" are different sentences and
 *   only one of them is honest when the read failed.
 * - [FailureBanner] when rows are already on screen. The stale rows stay; the
 *   banner says why there are no newer ones.
 * - [InlineFailure] when one section of a screen failed and the rest is fine.
 *
 * All three offer Retry when the failure is worth retrying, which is the part
 * that was missing: several screens named a problem and then left the owner
 * with no way to act on it but to leave and come back.
 */

/** The glyph for a cause, so the kind is legible before the words are read. */
private val Failure.icon: ImageVector
    get() = when (kind) {
        FailureKind.Offline -> Icons.Outlined.WifiOff
        FailureKind.Unreachable -> Icons.Outlined.CloudOff
        FailureKind.Auth -> Icons.Outlined.Lock
        FailureKind.NotFound -> Icons.AutoMirrored.Outlined.HelpOutline
        FailureKind.ServerFault, FailureKind.Garbled, FailureKind.Unknown -> Icons.Outlined.ErrorOutline
    }

/**
 * A whole pane, for when the read produced nothing to show.
 *
 * [onOpenSettings] is offered only when the fix is actually there — a rejected
 * credential or an unresolvable host. Offering it for a stopped server would
 * send somebody to edit settings that were already correct.
 */
@Composable
fun FailurePane(
    failure: Failure,
    onRetry: (() -> Unit)? = null,
    onOpenSettings: (() -> Unit)? = null,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier.fillMaxSize().padding(32.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Icon(failure.icon, null, tint = Muted, modifier = Modifier.size(40.dp))
        Spacer(Modifier.height(12.dp))
        Text(
            failure.headline,
            color = Ink, fontSize = 15.sp, fontWeight = androidx.compose.ui.text.font.FontWeight.SemiBold,
            textAlign = TextAlign.Center,
        )
        // A failure carrying only a sentence — a refusal the server worded
        // itself — has no second line, and an empty one leaves a gap that
        // reads as something failing to load.
        if (failure.detail.isNotBlank()) {
            Spacer(Modifier.height(6.dp))
            Text(
                failure.detail,
                color = Muted, fontSize = 12.sp, lineHeight = 17.sp,
                textAlign = TextAlign.Center,
            )
        }
        if (failure.retryable && onRetry != null) {
            Button(shape = MagicanButtonShape,
                onClick = onRetry,
                colors = ButtonDefaults.buttonColors(containerColor = Coral),
                modifier = Modifier.padding(top = 16.dp),
            ) { Text("Try again") }
        }
        if (failure.setupRequired && onOpenSettings != null) {
            TextButton(onClick = onOpenSettings) { Text("Open Settings", color = Coral) }
        }
    }
}

/**
 * A strip above content that is already on screen.
 *
 * The rows below it are kept deliberately: emptying a list somebody is part way
 * down, because a refresh failed, loses their place to tell them something they
 * could have been told in a bar.
 */
@Composable
fun FailureBanner(
    failure: Failure,
    onRetry: (() -> Unit)? = null,
    modifier: Modifier = Modifier,
) {
    Surface(color = Danger.copy(alpha = 0.10f), modifier = modifier.fillMaxWidth()) {
        Row(
            Modifier.padding(horizontal = 14.dp, vertical = 9.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(failure.icon, null, tint = Danger, modifier = Modifier.size(17.dp))
            Column(Modifier.padding(start = 10.dp).weight(1f)) {
                Text(
                    failure.headline,
                    color = Danger, fontSize = 12.sp,
                    fontWeight = androidx.compose.ui.text.font.FontWeight.SemiBold,
                )
                if (failure.detail.isNotBlank()) {
                    Text(failure.detail, color = Muted, fontSize = 11.sp, lineHeight = 15.sp)
                }
            }
            if (failure.retryable && onRetry != null) {
                TextButton(onClick = onRetry) { Text("Retry", color = Coral, fontSize = 13.sp) }
            }
        }
    }
}

/**
 * One section of a screen, where the rest of the screen is fine.
 *
 * Compact on purpose: a meeting list that could not be read should not push the
 * recording button off the screen.
 */
@Composable
fun InlineFailure(
    failure: Failure,
    onRetry: (() -> Unit)? = null,
    modifier: Modifier = Modifier,
) {
    Row(modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        Icon(failure.icon, null, tint = Danger, modifier = Modifier.size(15.dp))
        Text(
            failure.headline,
            color = Danger, fontSize = 12.sp,
            modifier = Modifier.padding(start = 8.dp).weight(1f),
        )
        if (failure.retryable && onRetry != null) {
            TextButton(onClick = onRetry) { Text("Retry", color = Coral, fontSize = 12.sp) }
        }
    }
}
