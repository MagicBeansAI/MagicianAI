package ai.magicbeans.magdroid.tutor

import ai.magicbeans.magdroid.screenshot.ScreenshotPipeline
import ai.magicbeans.magdroid.service.MagdroidAccessibilityService
import ai.magicbeans.magdroid.service.ScreenshotQuality
import android.graphics.BitmapFactory
import android.util.Log
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.withContext

/**
 * The live screen, for a lesson about what is in front of you.
 *
 * The companion can already capture — that is how it automates a phone — so
 * this borrows the same path rather than opening a second one. It builds its
 * own pipeline against the running service instead of reaching into the
 * service's private one, so the two do not have to be edited together.
 *
 * The capture route matters: `AccessibilityService.takeScreenshot` needs no
 * per-capture consent, because the accessibility grant already covers reading
 * the screen. MediaProjection would prompt and leave a cast indicator running,
 * which for "explain this button to me" is an absurd amount of ceremony.
 */
object TutorScreenGrab {

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    /** Whether a capture can be taken at all: the service has to be running. */
    fun available(): Boolean = MagdroidAccessibilityService.instance != null

    /**
     * Take one frame and hand it to [ScreenCapture].
     *
     * Returns whether it worked. A failed capture is reported rather than
     * thrown: the caller's next move is to teach on a blackboard instead, not
     * to abandon the question.
     */
    /**
     * One screen as JPEG bytes, for a caller that wants the image rather than
     * the tutor's target resolution. Null when the service is not running.
     */
    suspend fun jpeg(): ByteArray? = withContext(Dispatchers.IO) {
        val service = MagdroidAccessibilityService.instance ?: return@withContext null
        runCatching { ScreenshotPipeline(service, scope).capture(ScreenshotQuality.FULL) }
            .getOrNull()
    }

    suspend fun grab(): Boolean = withContext(Dispatchers.IO) {
        val service = MagdroidAccessibilityService.instance
        if (service == null) {
            ScreenCapture.fail("The companion's accessibility service is not running.")
            return@withContext false
        }
        runCatching {
            // Full quality: the tutor resolves targets from this image, and a
            // heavily compressed screenshot loses exactly the small text and
            // thin controls it needs to point at.
            val jpeg = ai.magicbeans.magdroid.bridge.BridgeLog.timed(TAG, "screen capture") {
                ScreenshotPipeline(service, scope).capture(ScreenshotQuality.FULL)
            }
            BitmapFactory.decodeByteArray(jpeg, 0, jpeg.size)
        }.fold(
            onSuccess = { bitmap ->
                if (bitmap == null) {
                    ScreenCapture.fail("The screen could not be read.")
                    false
                } else {
                    // The foreground package, read at the moment of capture.
                    // Read later it would name whatever the phone drifted to,
                    // which for an agent about to tap is worse than not knowing.
                    ScreenCapture.offer(bitmap, foregroundPackage())
                    true
                }
            },
            onFailure = { problem ->
                ai.magicbeans.magdroid.bridge.BridgeLog.warn(TAG, "screen grab failed: ${problem.message}")
                ScreenCapture.fail(problem.message ?: "The screen could not be captured.")
                false
            },
        )
    }

    /**
     * What is in front, right now.
     *
     * Straight from the accessibility service's own view of the active window
     * — the same thing that will be tapped, so there is no window in which the
     * answer and the target disagree.
     */
    private fun foregroundPackage(): String? {
        val root = MagdroidAccessibilityService.instance?.rootInActiveWindow ?: return null
        return try {
            root.packageName?.toString()
        } finally {
            // Required by the supported Android 7–12 pool; a no-op from 13.
            @Suppress("DEPRECATION")
            root.recycle()
        }
    }

    private const val TAG = "MagicianTutorGrab"
}
