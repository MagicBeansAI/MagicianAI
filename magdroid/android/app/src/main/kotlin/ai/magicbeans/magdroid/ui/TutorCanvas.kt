package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.tutor.TutorShape
import ai.magicbeans.magdroid.ui.TutorShapeRenderer.renderTutorShape
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.rememberTextMeasurer

/**
 * A storyboard, drawn.
 *
 * Shared by both canvases: the blackboard inside the app and the overlay drawn
 * over other apps. The shapes and their timing are identical in each — only
 * what sits behind them differs — so the drawing lives in one place rather than
 * being reimplemented per surface and drifting.
 */
@Composable
fun TutorCanvas(
    shapes: List<TutorShape>,
    /** How far through the storyboard, 0…1. Owned by the caller so it can scrub. */
    progress: Float,
    modifier: Modifier = Modifier,
) {
    val measurer = rememberTextMeasurer()

    // Ordered once per storyboard, not per frame. `revealOrder` is the
    // storyboard's own sequence and takes precedence over array order, because
    // a generator that emits shapes grouped by kind still means them to appear
    // in teaching order.
    val ordered = remember(shapes) {
        shapes.sortedBy { it.revealOrder ?: Int.MAX_VALUE }
    }
    val schedule = remember(ordered) { scheduleOf(ordered) }

    Canvas(modifier.fillMaxSize()) {
        ordered.forEachIndexed { index, shape ->
            val window = schedule[index]
            val local = window.progressAt(progress)
            // Not yet its turn: skipped rather than drawn at zero, so a shape
            // with no size of its own cannot flash a dot before it begins.
            if (local <= 0f) return@forEachIndexed
            renderTutorShape(shape, size, local, measurer)
        }
    }
}

/**
 * When each shape draws, as a fraction of the whole storyboard.
 *
 * Built from the shapes' own `delayMs` and `durationMs` where they carry them,
 * and evenly divided where they do not. A storyboard that specifies nothing
 * still animates in order rather than appearing at once, which is the
 * difference between a lesson and a diagram.
 */
internal fun scheduleOf(shapes: List<TutorShape>): List<RevealWindow> {
    if (shapes.isEmpty()) return emptyList()

    val explicit = shapes.any { it.delayMs != null || it.durationMs != null }
    if (!explicit) {
        val slice = 1f / shapes.size
        return shapes.indices.map { RevealWindow(it * slice, (it + 1) * slice) }
    }

    // Laid end to end on the storyboard's own clock, then normalised. Using the
    // declared milliseconds directly would make a long lesson play at the same
    // speed as a short one only by accident.
    var cursor = 0.0
    val spans = shapes.map { shape ->
        val start = cursor + (shape.delayMs ?: 0.0)
        val end = start + (shape.durationMs ?: DEFAULT_STEP_MS)
        cursor = end
        start to end
    }
    val total = spans.lastOrNull()?.second?.takeIf { it > 0 } ?: 1.0
    return spans.map { (start, end) ->
        RevealWindow((start / total).toFloat(), (end / total).toFloat())
    }
}

/** One shape's slice of the storyboard's timeline. */
internal data class RevealWindow(val start: Float, val end: Float) {
    /**
     * How far this shape has drawn, given the storyboard's overall progress.
     *
     * Clamped at both ends: a shape whose turn has passed stays finished rather
     * than restarting, and one whose turn has not come reports nothing rather
     * than a negative fraction the renderer would have to guard against.
     */
    fun progressAt(overall: Float): Float {
        if (overall <= start) return 0f
        if (overall >= end) return 1f
        val span = (end - start).takeIf { it > 0f } ?: return 1f
        return ((overall - start) / span).coerceIn(0f, 1f)
    }
}

private const val DEFAULT_STEP_MS = 700.0

/**
 * Plays a storyboard once, at its own pace.
 *
 * Separate from [TutorCanvas] so the canvas can also be driven by something
 * else — a scrubber, a replay, or the step machine advancing on the backend's
 * word rather than on a clock.
 */
@Composable
fun rememberStoryboardProgress(shapes: List<TutorShape>, playing: Boolean = true): Float {
    val durationMs = remember(shapes) { totalMillis(shapes) }
    val progress by animateFloatAsState(
        targetValue = if (playing) 1f else 0f,
        animationSpec = tween(durationMillis = durationMs, easing = LinearEasing),
        label = "storyboard",
    )
    return progress
}

internal fun totalMillis(shapes: List<TutorShape>): Int {
    if (shapes.isEmpty()) return 0
    val declared = shapes.sumOf { (it.delayMs ?: 0.0) + (it.durationMs ?: DEFAULT_STEP_MS) }
    // Floored rather than trusted: a storyboard that declares two seconds for
    // twelve shapes is unreadable, and one that declares nothing would finish
    // instantly.
    return declared.coerceAtLeast(shapes.size * 400.0).toInt()
}
