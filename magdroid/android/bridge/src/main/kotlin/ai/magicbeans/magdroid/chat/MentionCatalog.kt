package ai.magicbeans.magdroid.chat

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/**
 * The composer's `@`-mention catalog.
 *
 * A Kotlin port of `magios/Shared/MentionCatalog.swift`, which is itself a port
 * of the web's `composerMentions.ts`. The picker is a pure pipeline — group,
 * needle, filter, cap — so all three clients rank the same way and, more
 * importantly, insert the **same serialized token**. The backend parses that
 * token inline; a client that spells it differently is a client whose mentions
 * silently do nothing.
 */
enum class MentionKind { Agent, Tool, Personality, Task, Feature }

data class MentionItem(
    val id: String,
    val label: String,
    val detail: String? = null,
    val kind: MentionKind,
    /** The routing form the backend reads. */
    val insertText: String,
    val searchText: String? = null,
    /** Shown inside the chip while [insertText] keeps the precise id or slug. */
    val chipLabel: String? = null,
) {
    /**
     * Exactly what gets spliced into the composer.
     *
     * Feature lanes emit their marker rather than their id, because the marker
     * is what the backend's tokenizer looks for in the message text.
     */
    fun serialized(): String = when (id) {
        "feature:copilot" -> "@copilot"
        "feature:brainstorm" -> "@brainstorm"
        "feature:tutor" -> "@tutor"
        "feature:tutor_quick" -> "@tutor #quick"
        "feature:vibedev" -> "@vibedev"
        // The ASCII space is load-bearing: `#` is a token character in the
        // backend tokenizer, so a glued `@vibedev#discuss` is one unknown token
        // and matches no marker at all.
        "feature:vibedev_discuss" -> "@vibedev #discuss"
        else -> insertText
    }
}

sealed interface MentionGroup {
    data class Kind(val kind: MentionKind) : MentionGroup
    data object All : MentionGroup
}

/** A detected `@…` trigger: how much to replace, and what to search for. */
data class MentionTrigger(val consume: Int, val query: String)

object MentionCatalog {

    /** How many matches the picker will show. */
    const val LIMIT = 9

    /**
     * Which lane a query narrows to.
     *
     * Both singular and plural are accepted because people type both, and a
     * picker that shows nothing for `@agents` teaches that the feature is
     * broken rather than that the spelling was wrong.
     */
    fun groupForQuery(query: String): MentionGroup {
        val n = query.lowercase()
        fun matches(vararg words: String) = words.any { n == it || n.startsWith("$it:") }
        return when {
            matches("agent", "agents") -> MentionGroup.Kind(MentionKind.Agent)
            matches("skill", "skills", "tool", "tools") -> MentionGroup.Kind(MentionKind.Tool)
            matches("personality") -> MentionGroup.Kind(MentionKind.Personality)
            matches("task", "tasks") -> MentionGroup.Kind(MentionKind.Task)
            matches("feature", "features") -> MentionGroup.Kind(MentionKind.Feature)
            else -> MentionGroup.All
        }
    }

    /** What to search for, once the lane has been taken off the front. */
    fun needleForQuery(query: String, group: MentionGroup): String {
        if (group is MentionGroup.All) return query.lowercase()
        val colon = query.indexOf(':')
        return if (colon >= 0) query.substring(colon + 1).lowercase() else ""
    }

    fun filter(items: List<MentionItem>, group: MentionGroup, needle: String): List<MentionItem> =
        items.filter { item ->
            if (group is MentionGroup.Kind && item.kind != group.kind) return@filter false
            if (needle.isEmpty()) return@filter true
            val haystack = item.searchText ?: "${item.label} ${item.detail.orEmpty()} ${item.id}"
            haystack.lowercase().contains(needle)
        }

    /** group → needle → filter → cap. The single entry point. */
    fun matchesFor(items: List<MentionItem>, query: String, limit: Int = LIMIT): List<MentionItem> {
        val group = groupForQuery(query)
        return filter(items, group, needleForQuery(query, group)).take(limit)
    }

    private val commandForm = Regex(
        """(^|\s)@(agent|agents|skill|skills|tool|tools|personality|task|tasks|feature|features)\s+([A-Za-z0-9_:-]*)$""",
        RegexOption.IGNORE_CASE,
    )
    private val bareForm = Regex("""(^|\s)@([A-Za-z0-9_:-]*)$""")

    /**
     * Find a trailing `@…` before the caret.
     *
     * Two shapes: the command form `@agent foo`, and the bare form `@que`. The
     * bare form stops at a space, so prose written after a mention closes the
     * picker instead of leaving it open over the rest of the sentence.
     */
    fun detectTrigger(beforeCursor: String): MentionTrigger? {
        commandForm.find(beforeCursor)?.let { match ->
            val lead = match.groupValues[1]
            val kind = match.groupValues[2].lowercase()
            val tail = match.groupValues[3]
            // Everything from the `@` to the caret is replaced, so the lead
            // whitespace that anchored the match is preserved.
            val consume = beforeCursor.length - (match.range.first + lead.length)
            return MentionTrigger(consume = consume, query = "$kind:$tail")
        }
        bareForm.find(beforeCursor)?.let { match ->
            val query = match.groupValues[2]
            return MentionTrigger(consume = query.length + 1, query = query)
        }
        return null
    }

    fun kindLabel(kind: MentionKind): String = when (kind) {
        MentionKind.Agent -> "Agent"
        MentionKind.Personality -> "Personality"
        MentionKind.Task -> "Task"
        MentionKind.Feature -> "Feature"
        MentionKind.Tool -> "Tool"
    }

    /**
     * The feature lanes this client can honour.
     *
     * iOS's rule is that a lane appears only when the client executes it
     * natively or the server dispatches it from the marker alone. Sorted by
     * that rule rather than by iOS's list, Android gets four of the five.
     *
     * VibeDev and Tutor are both server-recognised: the marker rides along in
     * the message text and the backend acts on it, so any surface whose text
     * reaches chat gets the lane. Tutor from here drives the desktop gateway
     * rather than a canvas on the phone — `source_surface` decides the overlay
     * target, and only `ios_tutor_overlay` is drawn on the device that asked.
     * That is a real thing to want from a phone, so the lane is offered and its
     * description says where the answer appears.
     *
     * Thinking Map is the other kind. Nothing server-side recognises
     * `@brainstorm`; iOS intercepts it and opens a native canvas. Typed here it
     * would be sent as literal text and read as a stray word, so it is left out
     * — the one case where offering a lane does harm rather than merely
     * disappointing.
     */
    fun featureItems(): List<MentionItem> = listOf(
        // Android-only, and the reason is the platform: iOS can neither
        // screenshot another app nor tap in one, so this lane has no iOS
        // counterpart to be in parity with. On the web the operator drives a
        // Mac; here it drives the phone in your hand.
        MentionItem(
            id = "feature:copilot",
            label = "@copilot",
            detail = "App Copilot — does it on this phone, rather than showing you",
            kind = MentionKind.Feature,
            insertText = "feature:copilot",
            searchText = "copilot app copilot @copilot do it for me automate tap pilot feature",
            chipLabel = "@copilot",
        ),
        // Left out once, on the grounds that no server-side lane answers to
        // this name. True, and beside the point: it is a *client* lane on both
        // phones — the invocation never leaves the device, it opens a map — and
        // a composer that will not offer it is missing a feature.
        MentionItem(
            id = "feature:brainstorm",
            label = "@brainstorm",
            detail = "Thinking Map — grow an idea on a live canvas",
            kind = MentionKind.Feature,
            insertText = "feature:brainstorm",
            searchText = "brainstorm brainstrom ideas thinking map canvas weave feature",
            chipLabel = "@brainstorm",
        ),
        MentionItem(
            id = "feature:tutor",
            label = "@tutor",
            detail = "Tutor — teaches on your computer's screen",
            kind = MentionKind.Feature,
            insertText = "feature:tutor",
            searchText = "tutor @tutor teach explain blackboard overlay feature",
            chipLabel = "@tutor",
        ),
        MentionItem(
            id = "feature:tutor_quick",
            label = "@tutor_quick",
            detail = "Tutor, quick — the fastest first answer",
            kind = MentionKind.Feature,
            insertText = "feature:tutor_quick",
            searchText = "tutor quick tutor_quick @tutor #quick fast feature",
            chipLabel = "@tutor_quick",
        ),
        MentionItem(
            id = "feature:vibedev",
            label = "@vibedev",
            detail = "VibeDev — starts a build in your project",
            kind = MentionKind.Feature,
            insertText = "feature:vibedev",
            searchText = "vibedev @vibedev build implement ship develop feature",
            chipLabel = "@vibedev",
        ),
        MentionItem(
            id = "feature:vibedev_discuss",
            label = "@vibedev_discuss",
            detail = "VibeDev, discuss — plans the build without writing it",
            kind = MentionKind.Feature,
            insertText = "feature:vibedev_discuss",
            searchText = "vibedev discuss vibedev_discuss @vibedev #discuss plan spec design feature",
            chipLabel = "@vibedev_discuss",
        ),
    )

    /**
     * The whole list: features first so a bare `@` surfaces them, then agents,
     * then skills and personalities, then tasks.
     */
    fun build(
        agents: List<ReferenceAgent> = emptyList(),
        skills: List<ReferenceSkill> = emptyList(),
        tasks: List<Pair<String, String>> = emptyList(),
    ): List<MentionItem> {
        val items = mutableListOf<MentionItem>()
        items += featureItems()

        val seen = mutableSetOf<String>()
        for (agent in agents) {
            val id = agent.agentId.trim()
            if (id.isEmpty() || !seen.add(id)) continue
            val name = agent.name?.trim()
            val routeLabel = if (agent.route == "self") "Current agent" else "Delegate target"
            items += MentionItem(
                id = "agent:$id",
                label = if (!name.isNullOrEmpty()) "$name · $id" else id,
                detail = truncate(listOfNotNull(routeLabel, agent.description).filter { it.isNotEmpty() }.joinToString(" · ")),
                kind = MentionKind.Agent,
                insertText = "agent:$id",
                searchText = listOfNotNull(id, name, agent.description, routeLabel).joinToString(" "),
            )
        }

        for (skill in skills) {
            val name = skill.name.trim()
            if (name.isEmpty()) continue
            val isPersonality = skill.kind == "personality-mode"
            val ownerId = skill.ownerAgentId?.trim()
            val ownerName = skill.ownerAgentName?.trim()
            val delegated = !isPersonality && !ownerId.isNullOrEmpty() && skill.route == "delegate"
            val routeLabel = if (delegated) "Via ${ownerName?.takeIf { it.isNotEmpty() } ?: ownerId}" else null
            val fallback = when {
                isPersonality -> "Personality mode"
                skill.kind == "compiled" -> "Compiled tool"
                else -> "Procedure skill"
            }
            items += MentionItem(
                id = when {
                    delegated -> "tool:$name:via:$ownerId"
                    isPersonality -> "personality:$name"
                    else -> "tool:$name"
                },
                label = name,
                detail = truncate(
                    listOfNotNull(routeLabel, skill.description ?: fallback)
                        .filter { it.isNotEmpty() }.joinToString(" · "),
                ),
                kind = if (isPersonality) MentionKind.Personality else MentionKind.Tool,
                // A delegated skill names the agent it goes through; dropping
                // that would route it to whoever happens to be listening.
                insertText = when {
                    isPersonality -> "personality:$name"
                    delegated -> "skill:$name via agent:$ownerId"
                    else -> "skill:$name"
                },
                searchText = listOfNotNull(
                    name, skill.description, skill.kind, skill.layer, ownerId, ownerName, routeLabel,
                ).joinToString(" "),
            )
        }

        for ((id, title) in tasks) {
            if (id.isBlank() || title.isBlank()) continue
            items += MentionItem(
                id = "task:$id",
                label = title,
                detail = id,
                kind = MentionKind.Task,
                insertText = "task:$id",
                searchText = "$title $id",
                chipLabel = title,
            )
        }
        return items
    }

    private fun truncate(text: String, limit: Int = 80): String =
        if (text.length <= limit) text else text.take(limit - 1) + "…"
}

/**
 * `GET /chat/sessions/{id}/reference-catalog`.
 *
 * Agents and skills, in two lists. The client previously modelled this as a
 * flat `references` array, which the endpoint has never sent.
 */
@Serializable
data class ReferenceCatalogResponse(
    val agents: List<ReferenceAgent> = emptyList(),
    val skills: List<ReferenceSkill> = emptyList(),
)

@Serializable
data class ReferenceAgent(
    @SerialName("agent_id") val agentId: String = "",
    val name: String? = null,
    val description: String? = null,
    val route: String? = null,
)

@Serializable
data class ReferenceSkill(
    val name: String = "",
    val description: String? = null,
    val kind: String? = null,
    val layer: String? = null,
    @SerialName("owner_agent_id") val ownerAgentId: String? = null,
    @SerialName("owner_agent_name") val ownerAgentName: String? = null,
    val route: String? = null,
)
