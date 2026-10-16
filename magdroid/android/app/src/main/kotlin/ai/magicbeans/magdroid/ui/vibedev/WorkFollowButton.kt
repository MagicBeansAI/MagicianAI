package ai.magicbeans.magdroid.ui.vibedev

import ai.magicbeans.magdroid.vibedev.WorkFollows
import android.content.Context
import android.widget.Toast
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

/**
 * Follow / Unfollow a run, crew or task ([itemId] is `run:…`, `crew:…` or
 * `task:…`): a followed item gets its own ongoing notification until it ends.
 * At most three per scope; the server's refusal past that is shown through
 * [onNotice] (a toast by default).
 */
@Composable
internal fun WorkFollowButton(
    itemId: String,
    modifier: Modifier = Modifier,
    onNotice: ((String) -> Unit)? = null,
) {
    val context = LocalContext.current.applicationContext
    val followed by remember(context) { WorkFollows.followed(context) }.collectAsStateWithLifecycle()
    var busy by remember(itemId) { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    val on = itemId in followed
    VibeButton(
        if (on) "Unfollow" else "Follow",
        onClick = {
            if (!busy) {
                busy = true
                toggleFollow(scope, context, itemId, follow = !on, onNotice) { busy = false }
            }
        },
        modifier = modifier.semantics { stateDescription = if (on) "Following" else "Not following" },
        busy = busy,
    )
}

/** Follow or unfollow [itemId]; a failure's message goes to [onNotice] or a toast. */
internal fun toggleFollow(
    scope: CoroutineScope,
    context: Context,
    itemId: String,
    follow: Boolean,
    onNotice: ((String) -> Unit)?,
    onDone: () -> Unit = {},
) {
    scope.launch {
        val result = if (follow) WorkFollows.follow(context, itemId) else WorkFollows.unfollow(context, itemId)
        onDone()
        result.exceptionOrNull()?.let { error ->
            val message = error.message ?: if (follow) "Couldn't follow this." else "Couldn't unfollow this."
            onNotice?.invoke(message) ?: Toast.makeText(context, message, Toast.LENGTH_SHORT).show()
        }
    }
}
