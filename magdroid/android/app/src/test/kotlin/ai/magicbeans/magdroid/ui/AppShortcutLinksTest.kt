package ai.magicbeans.magdroid.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * Launcher shortcut routing.
 *
 * The three entry points iOS gives Siri, reached here by long-pressing the app
 * icon. The parser is the testable half; the launcher's own plumbing is
 * Android's.
 *
 * The negatives carry the weight: this parser is offered every URI the activity
 * receives, including the task and monitor links it must leave alone. Claiming
 * one of those would send somebody opening a task straight to a new chat.
 */
class AppShortcutLinksTest {

    @Test
    fun `each shortcut host resolves to its destination`() {
        assertEquals(AppShortcutTarget.NewChat, AppShortcutLinks.parse("magican://new-chat"))
        assertEquals(AppShortcutTarget.AskTutor, AppShortcutLinks.parse("magican://tutor"))
        assertEquals(AppShortcutTarget.StartListening, AppShortcutLinks.parse("magican://listen"))
        assertEquals(AppShortcutTarget.StartTalking, AppShortcutLinks.parse("magican://talk"))
        assertEquals(AppShortcutTarget.OpenAttention, AppShortcutLinks.parse("magican://attention"))
        assertEquals(AppShortcutTarget.OpenToday, AppShortcutLinks.parse("magican://today"))
    }

    /** Trailing slashes are what a launcher or a hand-typed link tends to carry. */
    @Test
    fun `a trailing slash still resolves`() {
        assertEquals(AppShortcutTarget.NewChat, AppShortcutLinks.parse("magican://new-chat/"))
    }

    /**
     * The links that belong to somebody else. Both parsers are handed every
     * incoming URI, and each must take only its own.
     */
    @Test
    fun `task and monitor links are not shortcuts`() {
        assertNull(AppShortcutLinks.parse("magican://task/task_123"))
        assertNull(AppShortcutLinks.parse("magican://monitor/task_123?update=mu_1"))
        assertNull(AppShortcutLinks.parse("magican://pair/abc"))
        assertNull(AppShortcutLinks.parse("https://ios.example.com/tasks?selected=task_1"))
    }

    @Test
    fun `an unknown host or scheme is not a shortcut`() {
        assertNull(AppShortcutLinks.parse("magican://something-else"))
        assertNull(AppShortcutLinks.parse("https://magican.ai/new-chat"))
        assertNull(AppShortcutLinks.parse("not a uri at all"))
        assertNull(AppShortcutLinks.parse(null))
        assertNull(AppShortcutLinks.parse(""))
    }

    /**
     * Consume clears only what it was given. A target left set re-fires on every
     * recomposition, and for "new chat" that silently discards the session the
     * owner had just started typing in — so the wrong id must not clear it and
     * the right one must.
     */
    @Test
    fun `consuming clears the target, and only the matching one`() {
        AppShortcutLinks.request(AppShortcutTarget.NewChat)
        assertEquals(AppShortcutTarget.NewChat, AppShortcutLinks.target.value)

        AppShortcutLinks.consume(AppShortcutTarget.AskTutor)
        assertEquals(
            "a different target must not clear this one",
            AppShortcutTarget.NewChat,
            AppShortcutLinks.target.value,
        )

        AppShortcutLinks.consume(AppShortcutTarget.NewChat)
        assertNull(AppShortcutLinks.target.value)
    }
}
