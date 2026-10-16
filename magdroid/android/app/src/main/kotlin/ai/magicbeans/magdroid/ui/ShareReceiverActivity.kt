package ai.magicbeans.magdroid.ui

import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.provider.OpenableColumns
import androidx.activity.ComponentActivity
import java.io.ByteArrayOutputStream

/**
 * Magician as the share sheet's plain target: send anything here.
 *
 * Distinct from [TutorShareActivity], which is the *contextual* target — "ask
 * about this screenshot", answered with a lesson. iOS draws the same line, as a
 * Share Extension beside a separate `Magican Assist` Action Extension, and both
 * appear in the sheet. This one is the quiet half: whatever was shared lands in
 * the composer and waits to be asked about.
 *
 * Android registered for image types only, and only into the tutor flow, so a
 * link, a note, or a PDF could not be sent to Magician at all — the app was
 * absent from the share sheet for every one of them.
 */
class ShareReceiverActivity : ComponentActivity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val handed = readShare(intent)
        if (handed) {
            startActivity(
                Intent(this, ChatActivity::class.java)
                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP),
            )
        }
        // No UI of its own. A share target that stops to render something is a
        // share target that makes you wait to say the thing you already decided
        // to say; the composer is where the thought continues.
        finish()
    }

    /** Returns whether anything was actually handed over. */
    private fun readShare(intent: Intent?): Boolean {
        if (intent == null) return false
        val text = intent.getStringExtra(Intent.EXTRA_TEXT)?.trim().orEmpty()
        val files = streams(intent).mapNotNull { readAttachment(it) }

        if (text.isEmpty() && files.isEmpty()) return false
        ChatShareInbox.hand(text, files)
        return true
    }

    /** Every shared stream, under either the single or multiple action. */
    private fun streams(intent: Intent): List<Uri> = when (intent.action) {
        Intent.ACTION_SEND -> listOfNotNull(parcelable(intent, Intent.EXTRA_STREAM))
        Intent.ACTION_SEND_MULTIPLE -> parcelableList(intent, Intent.EXTRA_STREAM)
        else -> emptyList()
    }.take(MAX_ATTACHMENTS)

    private fun parcelable(intent: Intent, key: String): Uri? =
        if (Build.VERSION.SDK_INT >= 33) {
            intent.getParcelableExtra(key, Uri::class.java)
        } else {
            @Suppress("DEPRECATION")
            intent.getParcelableExtra(key)
        }

    private fun parcelableList(intent: Intent, key: String): List<Uri> =
        if (Build.VERSION.SDK_INT >= 33) {
            intent.getParcelableArrayListExtra(key, Uri::class.java).orEmpty()
        } else {
            @Suppress("DEPRECATION")
            intent.getParcelableArrayListExtra<Uri>(key).orEmpty()
        }

    /**
     * Read one shared stream into memory.
     *
     * Bounded, because the sender decides the size and a share sheet will
     * happily hand over a video. Something too large is dropped rather than
     * taking the process down with it — the rest of the share still arrives.
     */
    private fun readAttachment(uri: Uri): SharedFile? = runCatching {
        val mime = contentResolver.getType(uri) ?: "application/octet-stream"
        val name = displayName(uri)
        val bytes = contentResolver.openInputStream(uri)?.use { stream ->
            val buffer = ByteArrayOutputStream()
            val chunk = ByteArray(64 * 1024)
            while (true) {
                val read = stream.read(chunk)
                if (read <= 0) break
                buffer.write(chunk, 0, read)
                if (buffer.size() > MAX_ATTACHMENT_BYTES) return@runCatching null
            }
            buffer.toByteArray()
        } ?: return@runCatching null
        if (bytes.isEmpty()) null else SharedFile(name, mime, bytes)
    }.getOrNull()

    private fun displayName(uri: Uri): String {
        val queried = runCatching {
            contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)
                ?.use { cursor -> if (cursor.moveToFirst()) cursor.getString(0) else null }
        }.getOrNull()
        return queried?.takeIf { it.isNotBlank() }
            ?: uri.lastPathSegment?.takeIf { it.isNotBlank() }
            ?: "shared-file"
    }

    private companion object {
        /** Matches what the composer will accept for a single turn. */
        const val MAX_ATTACHMENTS = 10
        const val MAX_ATTACHMENT_BYTES = 25 * 1024 * 1024
    }
}

/** One shared file, already read. */
data class SharedFile(val name: String, val mime: String, val bytes: ByteArray) {
    // Identity is the content, and a ByteArray compares by reference by
    // default — which would make two reads of the same file unequal.
    override fun equals(other: Any?): Boolean =
        this === other ||
            (
                other is SharedFile && name == other.name && mime == other.mime &&
                    bytes.contentEquals(other.bytes)
                )

    override fun hashCode(): Int =
        (name.hashCode() * 31 + mime.hashCode()) * 31 + bytes.contentHashCode()
}

/**
 * Shared content in transit between the share target and the composer.
 *
 * A process-lifetime handoff rather than a file, matching [TutorShareInbox]:
 * both sides are this process, and writing bytes to disk to read them back a
 * moment later would be work and a cleanup problem for no benefit. iOS needs a
 * real inbox on disk only because its extension is a separate process.
 */
object ChatShareInbox {

    /** What was shared, waiting for the composer to pick it up. */
    data class Pending(val text: String, val files: List<SharedFile>)

    @Volatile
    private var pending: Pending? = null

    fun hand(text: String, files: List<SharedFile>) {
        pending = Pending(text, files)
    }

    /** Take what is waiting, exactly once. */
    fun take(): Pending? {
        val held = pending
        pending = null
        return held
    }
}
