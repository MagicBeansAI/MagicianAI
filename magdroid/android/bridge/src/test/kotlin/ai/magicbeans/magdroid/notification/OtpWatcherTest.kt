package ai.magicbeans.magdroid.notification

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * A wrong code is worse than no code: it gets typed into a login form and burns
 * an attempt, and some services lock an account after a few. These tests are
 * mostly about what must NOT be matched.
 */
class OtpWatcherTest {

    private fun extract(text: String, title: String? = "Messages") =
        OtpWatcher.extract(title, text, "com.messaging", 1_000)?.code

    @Test
    fun `a plain verification message yields its code`() {
        assertEquals("482913", extract("482913 is your verification code"))
        assertEquals("5821", extract("Your OTP is 5821. Do not share it."))
        assertEquals("192837", extract("Use code 192837 to sign in"))
    }

    /** Senders split the phrase across title and body. */
    @Test
    fun `a code in the title is found when the intent word is in the body`() {
        assertEquals("774411", OtpWatcher.extract("774411", "is your login code", "p", 1)?.code)
    }

    /** "482 913" is one code, not two numbers. */
    @Test
    fun `a grouped code is read as one number`() {
        assertEquals("482913", extract("Your verification code is 482 913"))
        assertEquals("482913", extract("Login code: 482-913"))
    }

    /**
     * The important half. Nothing here is a one-time code, and matching any of
     * them would type the wrong digits into a real form.
     */
    @Test
    fun `messages that merely contain numbers are not codes`() {
        assertNull("no intent word", extract("Your order 482913 has shipped"))
        assertNull("no intent word", extract("Delivery arriving between 1400 and 1600"))
        assertNull("a year, not a code", extract("Your verification for 2024 is complete"))
        assertNull("placeholder digits", extract("Your code is 0000"))
        assertNull("no digits at all", extract("Your verification code has expired"))
    }

    /**
     * Two different numbers in one message is ambiguous, and guessing between
     * them is exactly how the wrong one gets used.
     */
    @Test
    fun `an ambiguous message is refused rather than guessed`() {
        assertNull(extract("Code 4821 replaces your earlier code 9930"))
    }

    /** The same code repeated is not ambiguous. */
    @Test
    fun `a repeated code is still a single answer`() {
        assertEquals("4821", extract("Your OTP is 4821. Enter 4821 to continue."))
    }

    /** A code that arrived before anyone asked is expired and misleading. */
    @Test
    fun `only notifications newer than the anchor are considered`() {
        val stale = NotificationInfo("com.sms", "Bank", "Your OTP is 111213", 500, false, true)
        val fresh = NotificationInfo("com.sms", "Bank", "Your OTP is 445566", 1_500, false, true)

        assertNull(OtpWatcher.newest(listOf(stale), since = 1_000))
        assertEquals("445566", OtpWatcher.newest(listOf(stale, fresh), since = 1_000)?.code)
    }

    /** With several fresh codes, the most recent one is the live one. */
    @Test
    fun `the newest code wins`() {
        val older = NotificationInfo("com.sms", "A", "Your code is 111213", 1_100, false, true)
        val newer = NotificationInfo("com.sms", "B", "Your code is 445566", 1_900, false, true)
        assertEquals("445566", OtpWatcher.newest(listOf(older, newer), since = 1_000)?.code)
    }
}

/**
 * Secure HITL P6: a challenge decides, not the newest code. Two live
 * challenges — or two different codes inside one window — are the person's
 * call; a code from before the window is never this challenge's.
 */
class OtpWatcherDecideTest {

    private fun notification(text: String, postedAt: Long, pkg: String = "com.messaging") =
        NotificationInfo(pkg, "Messages", text, postedAt, ongoing = false, clearable = true)

    @Test
    fun `one code inside the window is the decision`() {
        val decision = OtpWatcher.decide(
            listOf(notification("Your verification code is 482913", 1_000)),
            windowStartMs = 500,
            deadlineMs = 5_000,
            expectedDigits = 6,
        )
        assertEquals("482913", (decision as OtpWatcher.Decision.Code).found.code)
    }

    @Test
    fun `a code from before the window and one of the wrong length are not considered`() {
        val stale = notification("Your verification code is 482913", 100)
        val wrongLength = notification("Your login code is 4821", 1_200)
        assertEquals(OtpWatcher.Decision.None, OtpWatcher.decide(listOf(stale, wrongLength), 500, 5_000, 6))
        assertEquals(OtpWatcher.Decision.None, OtpWatcher.decide(listOf(notification("Your code is 918273", 9_000)), 500, 5_000, null))
    }

    @Test
    fun `two different codes inside the window are ambiguous, never the newest`() {
        val decision = OtpWatcher.decide(
            listOf(
                notification("Your verification code is 482913", 1_000),
                notification("Your verification code is 573920", 2_000, pkg = "com.other"),
            ),
            500, 5_000, 6,
        )
        assertEquals(OtpWatcher.Decision.Ambiguous(2), decision)
        // The same code twice is one decision.
        val repeated = OtpWatcher.decide(
            listOf(notification("Your verification code is 482913", 1_000), notification("Code 482913", 1_500)),
            500, 5_000, 6,
        )
        assertEquals("482913", (repeated as OtpWatcher.Decision.Code).found.code)
    }
}
