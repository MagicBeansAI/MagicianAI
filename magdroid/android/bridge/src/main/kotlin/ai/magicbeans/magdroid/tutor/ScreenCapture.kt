package ai.magicbeans.magdroid.tutor

import android.graphics.Bitmap
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * The live screen, for a lesson about what is actually in front of you.
 *
 * This is the half of the loop iOS cannot have. The web tutor works by looking
 * at the page and drawing on it; on a phone that needs a screenshot of whatever
 * app is open, and iOS gives an app no way to take one. Android does — through
 * the accessibility service that is already granted for automation, which takes
 * a frame with no per-capture prompt.
 *
 * The capture's size is carried with it and never assumed. Shapes come back in
 * the coordinate space of the image the tutor was shown, and the renderer
 * scales from that space onto the overlay — so an annotation lands on the real
 * control underneath rather than near it. Guessing the screen size instead
 * would put every arrow slightly off, which is worse than obviously wrong.
 */
object ScreenCapture {

    /**
     * A frame, the space its coordinates are in, and what was on screen.
     *
     * The package matters for Copilot rather than Tutor: teaching about a
     * screenshot needs only the picture, but acting on an app needs to know
     * which app, and asking the owner to name it when the phone already knows
     * is a question with an answer in the room.
     */
    data class Frame(
        val image: Bitmap,
        val width: Int,
        val height: Int,
        val packageName: String? = null,
    ) {
        /** The space to send with a tutor request, so shapes come back aligned. */
        fun space(): TutorSize = TutorSize(width.toDouble(), height.toDouble())
    }

    private val _latest = MutableStateFlow<Frame?>(null)
    val latest: StateFlow<Frame?> = _latest.asStateFlow()

    private val _problem = MutableStateFlow<String?>(null)
    val problem: StateFlow<String?> = _problem.asStateFlow()

    /**
     * Whether a capture can be taken at all.
     *
     * Set by whatever holds the accessibility service. Kept as a supplied
     * predicate rather than read here so this stays free of Android plumbing
     * and can be tested.
     */
    var available: () -> Boolean = { false }

    /**
     * Whether drawing over other apps is permitted.
     *
     * Supplied by the app, which owns the Context that can answer it. Defaults
     * to false so a caller that forgets to wire it falls back to the
     * blackboard rather than starting an overlay that cannot appear.
     */
    var overlayGranted: () -> Boolean = { false }

    /**
     * Hand in a frame that was just taken.
     *
     * The capture itself belongs to the accessibility service — it is the only
     * thing that can take one — so this holds the result rather than doing the
     * work, and the two do not have to know about each other.
     */
    fun offer(image: Bitmap, packageName: String? = null) {
        _problem.value = null
        _latest.value = Frame(image, image.width, image.height, packageName)
    }

    fun fail(reason: String) {
        _problem.value = reason
        _latest.value = null
    }

    /**
     * Take the frame, once.
     *
     * A lesson consumes the screen it was asked about. Leaving it behind would
     * let the next question be answered against a screen that has since
     * changed, which is the specific way this goes wrong: annotations landing
     * confidently on the wrong thing.
     */
    fun take(): Frame? {
        val held = _latest.value
        _latest.value = null
        return held
    }

    fun clear() {
        _latest.value = null
        _problem.value = null
    }
}
