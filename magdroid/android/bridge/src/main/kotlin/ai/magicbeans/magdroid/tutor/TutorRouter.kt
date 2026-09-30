package ai.magicbeans.magdroid.tutor

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * Where a lesson is drawn.
 *
 * iOS has one real choice here — its own overlay, or the paired desktop —
 * because it cannot draw over other apps. Android can, so `Overlay` means the
 * phone itself: the lesson lands on top of whatever is being taught.
 */
enum class TutorSurface(val wire: String, val label: String) {
    /** Over other apps, on this phone. Needs the draw-over-apps grant. */
    Overlay("android_tutor_overlay", "Over this phone"),

    /** Inside the app, on a blackboard. No grant, no lesson on top of anything. */
    Blackboard("android_tutor_blackboard", "On a blackboard"),

    /**
     * The paired computer, which is what a phone gets today without declaring a
     * surface of its own. Kept as a choice because teaching on the desktop from
     * the phone in your hand is a real thing to want.
     */
    Desktop("android", "On my computer"),
}

/**
 * Decides where a run draws, and holds the run while it does.
 *
 * Separate from both the canvas and the step machine so the decision can be
 * made once, at the start, from things neither of them knows: which surface the
 * owner chose, and whether the grant behind it is actually in place.
 */
class TutorRouter(
    /** Whether the draw-over-apps grant is held. Passed in; this has no Context. */
    private val overlayPermitted: () -> Boolean,
) {
    private val _surface = MutableStateFlow(TutorSurface.Overlay)
    val surface: StateFlow<TutorSurface> = _surface.asStateFlow()

    private val _run = MutableStateFlow<TutorRun?>(null)
    val run: StateFlow<TutorRun?> = _run.asStateFlow()

    private val _refusal = MutableStateFlow<String?>(null)
    val refusal: StateFlow<String?> = _refusal.asStateFlow()

    fun choose(surface: TutorSurface) {
        _surface.value = surface
        _refusal.value = null
    }

    /**
     * The surface a run will actually use.
     *
     * Falls back to the blackboard when the overlay is chosen without its
     * grant, rather than refusing the lesson. Somebody who asked a question
     * wants an answer; landing it in the app instead of over the app is a
     * smaller failure than not answering, and the refusal says why so the
     * grant can be fixed.
     */
    fun resolve(): TutorSurface {
        val chosen = _surface.value
        if (chosen == TutorSurface.Overlay && !overlayPermitted()) {
            _refusal.value =
                "Drawing over other apps is not permitted yet, so this lesson is on the blackboard."
            return TutorSurface.Blackboard
        }
        _refusal.value = null
        return chosen
    }

    /**
     * The `source_surface` this client announces when starting a run.
     *
     * It is what decides where the backend sends the actions, so it has to name
     * the surface that will actually draw them — not the one that was chosen
     * and then fell back.
     */
    fun sourceSurface(): String = resolve().wire

    /** Begin a run on the resolved surface. */
    fun begin(): TutorRun = TutorRun().also { _run.value = it }

    fun end() {
        _run.value = null
    }
}
