package ai.magicbeans.magdroid.realtime

/**
 * Which events a subscriber wants.
 *
 * Every set field must match; an unset field matches anything. The id fields
 * compare against [HubEvent.refs], which already falls back to the payload when
 * the server sent no `refs`. [eventTypes] is a prefix match against both the
 * outer `event_type` and an `AgentEvent`'s inner one, so `"tutor."` selects the
 * tutor's events wherever they ride.
 *
 * [categories] and [userRelevant] are server classifications. A frame from a
 * server that does not classify yet carries neither, and passes those two
 * clauses rather than being dropped: a missing stamp is not evidence the event
 * is irrelevant, and every subscriber still parses what it receives.
 *
 * [predicate] runs last, for the rules a field cannot say.
 */
data class HubFilter(
    val eventTypes: List<String>? = null,
    val categories: List<String>? = null,
    val taskId: String? = null,
    val executionId: String? = null,
    val chatTurnId: String? = null,
    val agentId: String? = null,
    val sessionId: String? = null,
    val userRelevant: Boolean? = null,
    val predicate: ((HubEvent) -> Boolean)? = null,
) {
    fun matches(event: HubEvent): Boolean {
        eventTypes?.let { prefixes ->
            val outer = event.eventType
            val inner = event.agentEventType
            if (prefixes.none { outer.startsWith(it) || (inner != null && inner.startsWith(it)) }) return false
        }
        categories?.let { wanted ->
            val stamped = event.categories
            if (stamped != null && wanted.none(stamped::contains)) return false
        }
        userRelevant?.let { wanted ->
            val stamped = event.userRelevant
            if (stamped != null && stamped != wanted) return false
        }
        if (taskId != null && event.refs.taskId != taskId) return false
        if (executionId != null && event.refs.executionId != executionId) return false
        if (chatTurnId != null && event.refs.chatTurnId != chatTurnId) return false
        if (agentId != null && event.refs.agentId != agentId) return false
        if (sessionId != null && event.refs.sessionId != sessionId) return false
        return predicate?.invoke(event) ?: true
    }

    companion object {
        /** Every event; the subscriber filters for itself. */
        val ALL = HubFilter()
    }
}
