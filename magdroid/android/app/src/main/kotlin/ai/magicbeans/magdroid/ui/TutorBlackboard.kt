package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.tutor.TutorShape
import android.graphics.Bitmap
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import ai.magicbeans.magdroid.voice.Speech
import ai.magicbeans.magdroid.voice.VoicePrefs
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * A lesson, drawn on a board rather than over the world.
 *
 * Two jobs in one surface: teaching about a shared screenshot, and teaching
 * about nothing in particular. The picture is optional because the tutor is
 * just as useful explaining an idea on an empty board as annotating something —
 * which is how iOS uses it when no screenshot was shared.
 *
 * The drawing is the same canvas the overlay uses. Only what sits behind it
 * differs, and that is the entire difference between the two surfaces.
 */
@Composable
fun TutorBlackboard(
    shapes: List<TutorShape>,
    progress: Float,
    caption: String?,
    subject: Bitmap? = null,
    onClose: () -> Unit,
) {
    // The caption is spoken as it changes, honouring the same mute the rest of
    // the app honours. A tutor that talks over a silenced phone is the one
    // thing worse than one that stays silent.
    val context = androidx.compose.ui.platform.LocalContext.current
    val speech = remember(context) { Speech(context) }
    val prefs = remember(context) { VoicePrefs.get(context) }
    val speakReplies by prefs.speakReplies.collectAsStateWithLifecycle()
    LaunchedEffect(caption) {
        caption?.takeIf { it.isNotBlank() && speakReplies }?.let { speech.speak(it) }
    }
    DisposableEffect(Unit) { onDispose { speech.shutdown() } }

    Box(Modifier.fillMaxSize().background(BOARD)) {
        // Behind the drawing and scaled to fit, so the shapes' coordinate space
        // and the picture's agree. Cropping would slide every annotation off
        // the thing it points at.
        subject?.let { picture ->
            Image(
                bitmap = picture.asImageBitmap(),
                contentDescription = null,
                contentScale = ContentScale.Fit,
                modifier = Modifier.fillMaxSize().padding(12.dp),
            )
        }

        TutorCanvas(shapes = shapes, progress = progress, modifier = Modifier.fillMaxSize())

        Column(
            Modifier.fillMaxWidth().align(Alignment.BottomStart).padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            // What the tutor is saying, in text as well as aloud. A lesson that
            // is only spoken is no lesson at all on a muted phone.
            caption?.takeIf { it.isNotBlank() }?.let { line ->
                Surface(color = Color.Black.copy(alpha = 0.55f), shape = androidx.compose.foundation.shape.RoundedCornerShape(10.dp)) {
                    Text(
                        line,
                        color = Color.White, fontSize = 14.sp, lineHeight = 19.sp,
                        modifier = Modifier.padding(horizontal = 14.dp, vertical = 10.dp),
                    )
                }
            }
            Surface(
                color = Color.White.copy(alpha = 0.14f),
                shape = androidx.compose.foundation.shape.RoundedCornerShape(10.dp),
                modifier = Modifier.clickable { onClose() },
            ) {
                Text(
                    "Done",
                    color = Color.White, fontSize = 14.sp, fontWeight = FontWeight.SemiBold,
                    modifier = Modifier.padding(horizontal = 18.dp, vertical = 10.dp),
                )
            }
            Spacer(Modifier.height(4.dp))
        }
    }
}

/**
 * The board's own colour, not the theme's.
 *
 * Every theme's background is chosen to sit behind text; a lesson is chalk on
 * something dark, and a pale theme would leave yellow annotations invisible.
 * This is the one surface that deliberately ignores the palette.
 */
private val BOARD = Color(0xFF14181A)
