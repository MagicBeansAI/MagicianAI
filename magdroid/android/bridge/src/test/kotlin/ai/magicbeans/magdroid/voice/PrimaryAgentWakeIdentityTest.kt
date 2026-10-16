package ai.magicbeans.magdroid.voice

import org.junit.Assert.assertEquals
import org.junit.Test

class PrimaryAgentWakeIdentityTest {
    @Test
    fun `canonical name and aliases become backend-compatible wake phrases`() {
        assertEquals(
            listOf("Hey Atlas", "Hey Nova", "Hey Sam the Magician"),
            activationPhrases(
                PrimaryAgentWakeIdentity(
                    agentId = "personal-assistant",
                    name = "Atlas",
                    aliases = listOf("Nova", "HEY nova", " Sam-the-Magician! "),
                ),
            ),
        )
    }

    @Test
    fun `missing or unusable identity invents no microphone gate`() {
        assertEquals(emptyList<String>(), activationPhrases(null))
        assertEquals(
            emptyList<String>(),
            activationPhrases(PrimaryAgentWakeIdentity("a", "  !! ", listOf("", "hey"))),
        )
    }

    @Test
    fun `primary identity decodes from the scoped agent envelope`() {
        val decoded = decodePrimaryAgent(
            """{"agents":[
                {"definition":{"agent_id":"worker","name":"Worker","is_primary":false}},
                {"definition":{"agent_id":"pa","name":"Atlas","aliases":["Nova"],"is_primary":true}}
            ]}""",
        )
        assertEquals(PrimaryAgentWakeIdentity("pa", "Atlas", listOf("Nova")), decoded)
    }

    @Test
    fun `wake spellings replace a name the recogniser cannot arm`() {
        assertEquals(
            listOf("Hey magical", "Hey magician"),
            activationPhrases(
                PrimaryAgentWakeIdentity(
                    agentId = "personal-assistant",
                    name = "Magican",
                    aliases = emptyList(),
                    wakeSpellings = listOf("magical", "magician"),
                ),
            ),
        )
    }

    @Test
    fun `blank wake spellings fall back to the name and aliases`() {
        assertEquals(
            listOf("Hey Magican"),
            activationPhrases(
                PrimaryAgentWakeIdentity(
                    agentId = "personal-assistant",
                    name = "Magican",
                    aliases = emptyList(),
                    wakeSpellings = listOf("  ", ""),
                ),
            ),
        )
    }

    @Test
    fun `wake spellings decode from the scoped agent envelope`() {
        val decoded = decodePrimaryAgent(
            """{"agents":[
                {"definition":{"agent_id":"pa","name":"Magican",
                 "wake_spellings":["magical","magician"],"is_primary":true}}
            ]}""",
        )
        assertEquals(listOf("magical", "magician"), decoded.wakeSpellings)
        assertEquals(listOf("Hey magical", "Hey magician"), activationPhrases(decoded))
    }

}
