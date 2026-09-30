package ai.magicbeans.magdroid.observe

import android.content.Context
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.PixelFormat
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.ImageReader
import android.media.projection.MediaProjection
import android.media.projection.MediaProjectionManager
import android.os.Handler
import android.os.Looper
import android.util.DisplayMetrics
import android.view.Display
import java.io.ByteArrayOutputStream

/**
 * The screen, as keyframes, while the owner is sharing it to a meeting.
 *
 * MediaProjection rather than the accessibility screenshot path, on purpose.
 * Sharing a screen into a meeting is the capability Android gates behind its
 * own consent dialog and status chip, and reaching it through the
 * accessibility service would take the system's disclosure away from the
 * owner. The projection lives exactly as long as the share: it is created
 * from a consent the owner just gave, and closing it retires the chip.
 *
 * A poll, not a stream. [latestJpeg] hands back a frame only when the screen
 * has drawn since the last call — an unchanged screen yields nothing, which
 * is the right amount to upload about it.
 */
class ScreenShareFrames private constructor(
    private val projection: MediaProjection,
    private val display: VirtualDisplay,
    private val reader: ImageReader,
) {

    /**
     * The newest frame since the last call, or null when nothing new drew.
     *
     * Best effort throughout: a frame that cannot be read or encoded is
     * skipped, because the next tick will take another and a share that
     * crashes over one bad buffer shares nothing at all.
     */
    fun latestJpeg(): ByteArray? {
        val image = runCatching { reader.acquireLatestImage() }.getOrNull() ?: return null
        return try {
            val bitmap = image.toBitmap()
            try {
                bitmap.jpeg(JPEG_QUALITY)
            } finally {
                bitmap.recycle()
            }
        } catch (_: Exception) {
            null
        } finally {
            image.close()
        }
    }

    /** Idempotent. Stopping the projection is what retires the system chip. */
    fun close() {
        runCatching { display.release() }
        runCatching { reader.close() }
        runCatching { projection.stop() }
    }

    companion object {
        private const val DISPLAY_NAME = "Magician-ScreenShare"

        /** Same as the observe screenshot path, so both halves read alike. */
        private const val JPEG_QUALITY = 80

        /**
         * Trade the consent the system dialog just granted for a live pipeline.
         *
         * The caller must already be a foreground service of the
         * mediaProjection type — Android 14 refuses the projection otherwise,
         * which is why the service promotes its type before calling this.
         *
         * [onRevoked] fires (on the main looper) when the projection ends from
         * outside — the system's own "stop sharing" affordance, or another app
         * taking the projection. It also fires as an echo of [close]; callers
         * make their teardown idempotent rather than distinguishing.
         */
        fun open(
            context: Context,
            resultCode: Int,
            data: Intent,
            onRevoked: () -> Unit,
        ): ScreenShareFrames? = runCatching {
            val manager = context.getSystemService(MediaProjectionManager::class.java)
            val projection = manager.getMediaProjection(resultCode, data) ?: return null
            // Mandatory on 14+ before any virtual display exists, and the hook
            // through which the system's stop-sharing chip reaches this share.
            projection.registerCallback(
                object : MediaProjection.Callback() {
                    override fun onStop() {
                        onRevoked()
                    }
                },
                Handler(Looper.getMainLooper()),
            )
            // Real metrics — the full panel, bars included — because a mirror
            // of part of the screen is a crop nobody asked for. The deprecated
            // read is the one that still answers from a service context.
            val display = context.getSystemService(DisplayManager::class.java)
                .getDisplay(Display.DEFAULT_DISPLAY) ?: return null
            val metrics = DisplayMetrics()
            @Suppress("DEPRECATION")
            display.getRealMetrics(metrics)

            val reader = ImageReader.newInstance(
                metrics.widthPixels, metrics.heightPixels, PixelFormat.RGBA_8888, 2,
            )
            val virtual = projection.createVirtualDisplay(
                DISPLAY_NAME,
                metrics.widthPixels, metrics.heightPixels, metrics.densityDpi,
                DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR,
                reader.surface,
                null,
                null,
            )
            ScreenShareFrames(projection, virtual, reader)
        }.getOrNull()
    }
}

/** RGBA plane to bitmap, trimming the row padding the buffer may carry. */
private fun android.media.Image.toBitmap(): Bitmap {
    val plane = planes[0]
    val pixelStride = plane.pixelStride
    val rowPadding = plane.rowStride - pixelStride * width
    val padded = Bitmap.createBitmap(
        width + rowPadding / pixelStride, height, Bitmap.Config.ARGB_8888,
    )
    padded.copyPixelsFromBuffer(plane.buffer)
    if (rowPadding == 0) return padded
    val exact = Bitmap.createBitmap(padded, 0, 0, width, height)
    padded.recycle()
    return exact
}

private fun Bitmap.jpeg(quality: Int): ByteArray =
    ByteArrayOutputStream()
        .also { compress(Bitmap.CompressFormat.JPEG, quality, it) }
        .toByteArray()
