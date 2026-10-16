package ai.magicbeans.magdroid.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `@brainstorm`, against `BrainstormInvoke` in `magios/Shared/MentionCatalog.swift`.
 *
 * A client lane: the invocation never reaches the backend, so nothing on the
 * server would catch a mistake here. Getting the boundary wrong sends somebody's
 * thought into chat as ordinary text, or swallows a sentence that merely
 * mentioned brainstorming.
 */
class BrainstormInvokeTest {

    @Test
    fun `a bare invocation is recognised and leaves nothing behind`() {
        assertTrue(BrainstormInvoke.isInvoke("@brainstorm"))
        // Empty is meaningful: it opens capture for a fresh map.
        assertEquals("", BrainstormInvoke.strip("@brainstorm"))
    }

    @Test
    fun `the seed is whatever followed the invocation`() {
        assertEquals("pricing tiers", BrainstormInvoke.strip("@brainstorm pricing tiers"))
        assertEquals("pricing tiers", BrainstormInvoke.strip("  @brainstorm   pricing tiers  "))
        assertEquals("pricing tiers", BrainstormInvoke.strip("@brainstorm: pricing tiers"))
        assertEquals("pricing tiers", BrainstormInvoke.strip("@brainstorm, pricing tiers"))
    }

    /**
     * The typo is accepted deliberately.
     *
     * iOS tolerates `@brainstrom` because people type it. Refusing it here
     * would make the same slip behave differently on two phones — the message
     * would go to chat instead of opening a map.
     */
    @Test
    fun `the common misspelling works too`() {
        assertTrue(BrainstormInvoke.isInvoke("@brainstrom an idea"))
        assertEquals("an idea", BrainstormInvoke.strip("@brainstrom an idea"))
    }

    @Test
    fun `case does not matter`() {
        assertTrue(BrainstormInvoke.isInvoke("@BrainStorm this"))
        assertEquals("this", BrainstormInvoke.strip("@BRAINSTORM this"))
    }

    /**
     * Leading only.
     *
     * A message that mentions brainstorming part way through is a message, and
     * hijacking it would lose what somebody was saying.
     */
    @Test
    fun `an invocation must lead the message`() {
        assertFalse(BrainstormInvoke.isInvoke("let us @brainstorm this"))
        assertFalse(BrainstormInvoke.isInvoke("thoughts on @brainstorm?"))
    }

    /**
     * A longer word is a different word.
     *
     * `@brainstorming` is not the lane, and swallowing it would take a message
     * the owner meant to send.
     */
    @Test
    fun `a longer word is not the invocation`() {
        assertFalse(BrainstormInvoke.isInvoke("@brainstorming session notes"))
        assertFalse(BrainstormInvoke.isInvoke("@brainstormed already"))
    }

    @Test
    fun `unrelated text is untouched`() {
        assertFalse(BrainstormInvoke.isInvoke("@tutor explain this"))
        assertFalse(BrainstormInvoke.isInvoke("brainstorm without the at sign"))
        assertFalse(BrainstormInvoke.isInvoke(""))
    }

    /** Only the leading invocation goes; a later mention is part of the seed. */
    @Test
    fun `strip removes one invocation, not every mention`() {
        assertEquals(
            "compare with @brainstorm mode",
            BrainstormInvoke.strip("@brainstorm compare with @brainstorm mode"),
        )
    }
}
