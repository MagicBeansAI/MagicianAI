package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.tutor.TutorSurface
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.net.Uri
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * Magican as a share target: screenshot something, share it here, ask about it.
 *
 * iOS builds this as an Action Extension that saves the image to a shared inbox
 * and reopens the host app with a token, because an extension is a separate
 * process that cannot hand a bitmap over directly. Android's share sheet
 * delivers the image straight to this activity, so there is no inbox, no token,
 * and no handoff to get wrong.
 *
 * The question is asked here rather than after launching the app. Somebody who
 * shared a screenshot is already looking at the thing they want explained, and
 * making them find the app and re-establish that context first is how a
 * two-second thought becomes a chore.
 */
class TutorShareActivity : ComponentActivity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        val shared = readSharedImage(intent)
        if (shared == null) {
            finish()
            return
        }

        setContent {
            val store = remember { ThemeStore.get(this) }
            SharePrompt(
                image = shared,
                onAsk = { question ->
                    startTutor(shared, question)
                    finish()
                },
                onCancel = ::finish,
            )
        }
    }

    /**
     * Begin a lesson over the shared screenshot.
     *
     * Handed to the blackboard rather than the overlay: the subject is the
     * picture, not the app in front of you, and drawing over a live screen to
     * explain a screenshot of a different one would teach the wrong thing.
     */
    private fun startTutor(image: Bitmap, question: String) {
        TutorShareInbox.hand(image, question, TutorSurface.Blackboard)
        startActivity(
            Intent(this, ChatActivity::class.java)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP),
        )
    }

    private fun readSharedImage(intent: Intent?): Bitmap? {
        if (intent?.action != Intent.ACTION_SEND) return null
        val uri: Uri? = if (android.os.Build.VERSION.SDK_INT >= 33) {
            intent.getParcelableExtra(Intent.EXTRA_STREAM, Uri::class.java)
        } else {
            @Suppress("DEPRECATION")
            intent.getParcelableExtra(Intent.EXTRA_STREAM)
        }
        return uri?.let { source ->
            runCatching {
                contentResolver.openInputStream(source)?.use { BitmapFactory.decodeStream(it) }
            }.getOrNull()
        }
    }
}

/**
 * The image and question, in transit between the share target and the app.
 *
 * A process-lifetime handoff rather than a file: both sides are this process,
 * and writing a bitmap to disk to read it back a moment later would be work and
 * a cleanup problem for no benefit.
 */
object TutorShareInbox {
    data class Pending(val image: Bitmap?, val question: String, val surface: TutorSurface)

    @Volatile
    private var pending: Pending? = null

    fun hand(image: Bitmap?, question: String, surface: TutorSurface) {
        pending = Pending(image, question, surface)
    }

    /** Taken once. A second reader gets nothing rather than a repeat lesson. */
    fun take(): Pending? {
        val held = pending
        pending = null
        return held
    }
}

@Composable
private fun SharePrompt(image: Bitmap, onAsk: (String) -> Unit, onCancel: () -> Unit) {
    var question by remember { mutableStateOf("") }

    Column(
        Modifier.fillMaxSize().background(Ground).padding(20.dp),
        verticalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        Text("Ask about this", color = Ink, fontSize = 20.sp, fontWeight = FontWeight.Bold)
        Image(
            bitmap = image.asImageBitmap(),
            contentDescription = null,
            contentScale = ContentScale.Fit,
            modifier = Modifier
                .fillMaxWidth()
                .heightIn(max = 320.dp)
                .clip(RoundedCornerShape(10.dp)),
        )
        Surface(color = Panel, shape = RoundedCornerShape(12.dp), modifier = Modifier.fillMaxWidth()) {
            BasicTextField(
                value = question,
                onValueChange = { question = it },
                textStyle = TextStyle(color = Ink, fontSize = 15.sp),
                cursorBrush = SolidColor(Coral),
                decorationBox = { field ->
                    if (question.isEmpty()) {
                        Text("What would you like explained?", color = Muted, fontSize = 15.sp)
                    }
                    field()
                },
                modifier = Modifier.fillMaxWidth().padding(14.dp),
            )
        }
        Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Surface(
                color = Coral,
                shape = RoundedCornerShape(10.dp),
                // An empty question is allowed: "explain this" is the obvious
                // default, and demanding words before a screenshot can be asked
                // about defeats the point of sharing one.
                modifier = Modifier.clickable { onAsk(question.ifBlank { "Explain this" }) },
            ) {
                Text(
                    "Teach me",
                    color = Color.White, fontSize = 15.sp, fontWeight = FontWeight.SemiBold,
                    modifier = Modifier.padding(horizontal = 20.dp, vertical = 12.dp),
                )
            }
            Surface(
                color = Panel,
                shape = RoundedCornerShape(10.dp),
                modifier = Modifier.clickable { onCancel() },
            ) {
                Text(
                    "Cancel",
                    color = Secondary, fontSize = 15.sp,
                    modifier = Modifier.padding(horizontal = 20.dp, vertical = 12.dp),
                )
            }
        }
        Spacer(Modifier.height(4.dp))
        Text(
            "The tutor draws on the screenshot and talks you through it.",
            color = Muted, fontSize = 11.sp, lineHeight = 15.sp,
        )
    }
}
