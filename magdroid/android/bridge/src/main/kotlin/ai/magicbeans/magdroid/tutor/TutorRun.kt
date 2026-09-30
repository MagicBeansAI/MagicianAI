package ai.magicbeans.magdroid.tutor

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

/**
 * A tutor run, as it arrives.
 *
 * The backend narrates a lesson as a stream of events: shapes to draw, steps it
 * has reached, and an end. This turns that stream into the two things a surface
 * needs — what should be on screen right now, and what to say — and nothing
 * else. Deciding *where* to draw belongs to the router; deciding *how* belongs
 * to the renderer.
 */
class TutorRun {

    private val json = Json { ignoreUnknownKeys = true; isLenient = true }

    /** What should currently be drawn, in arrival order. */
    private val live = mutableListOf<TutorShape>()

    // Observable, because the canvas is Compose and a plain list would draw
    // once and then never again. The state is the run's; the surface only
    // watches it.
    private val _shapes = MutableStateFlow<List<TutorShape>>(emptyList())
    val shapesFlow: StateFlow<List<TutorShape>> = _shapes.asStateFlow()

    private val _caption = MutableStateFlow<String?>(null)
    val captionFlow: StateFlow<String?> = _caption.asStateFlow()

    private val _finished = MutableStateFlow(false)
    val finishedFlow: StateFlow<Boolean> = _finished.asStateFlow()

    /** The last caption, for a surface that shows what the tutor is doing. */
    var caption: String?
        get() = _caption.value
        private set(value) { _caption.value = value }

    /** True once the run has finished and the canvas should clear. */
    var finished: Boolean
        get() = _finished.value
        private set(value) { _finished.value = value }

    val shapes: List<TutorShape> get() = live.toList()

    private fun publish() {
        _shapes.value = live.toList()
    }

    /**
     * Apply one event.
     *
     * Returns true when the visible state changed, so a caller can avoid
     * redrawing on the events that only narrate.
     */
    fun apply(rawEvent: String): Boolean {
        val event = runCatching { json.parseToJsonElement(rawEvent) as? JsonObject }.getOrNull()
            ?: return false
        val type = event["event_type"]?.jsonPrimitive?.content.orEmpty()
        val payload = event["payload"] as? JsonObject ?: JsonObject(emptyMap())

        return when (type) {
            "tutor.draw.shape" -> drawShape(payload)

            // Progress narration. These change what the surface says, not what
            // it draws, which is why the return value distinguishes them.
            "tutor.step.observed" -> narrate("Tutor observed the screen")
            "tutor.step.target_resolved" -> narrate("Tutor found the target")

            "tutor.run.completed" -> {
                finished = true
                live.clear()
                publish()
                caption = null
                true
            }

            else -> false
        }
    }

    private fun narrate(text: String): Boolean {
        caption = text
        return false
    }

    private fun drawShape(payload: JsonObject): Boolean {
        val shapeJson = payload["shape"] as? JsonObject ?: payload
        val shape = runCatching {
            json.decodeFromString(TutorShape.serializer(), shapeJson.toString())
        }.getOrNull() ?: return false

        // Lifecycle, matching the web and iOS. `clearPrevious` drops what came
        // before unless it was marked to persist, and a shape that was
        // persisting only until a given step goes when that step arrives.
        // Without these a long lesson accumulates every mark it ever drew.
        if (shape.clearPrevious == true) {
            live.retainAll { it.persist == true }
        }
        shape.storyboardStepId?.let { step ->
            live.retainAll { it.persistUntilStep != step }
        }

        live += shape
        publish()
        shape.narration?.takeIf { it.isNotBlank() }?.let { caption = it }
        return true
    }

    /** Start again, for a second run on the same surface. */
    fun reset() {
        live.clear()
        publish()
        caption = null
        finished = false
    }
}
