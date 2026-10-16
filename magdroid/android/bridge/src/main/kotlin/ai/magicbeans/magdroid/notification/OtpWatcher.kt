package ai.magicbeans.magdroid.notification

/**
 * Extracts a one-time code from a notification, and nothing else.
 *
 * `android_get_notifications` hands back every active notification in full —
 * mail previews, messages, whatever is on the phone. An agent that calls it to
 * collect a six-digit code also collects all of that, and whatever it read is
 * then in a model's context for the rest of the session.
 *
 * The actual need is one short number. This narrows the capability to that.
 *
 * The rules are the runtime's own (`verification_codes::extract` in the
 * Magician tree) — one rule set, two runtimes — and both are checked against
 * the same fixture list (`extraction_fixtures.json`): approved formats only
 * (4–8 digits, one run or equal groups joined by a space, a dash or a dot),
 * a verification cue nearby, links cut out, recovery/setup/reset/promotion
 * wording refused, two distinct candidates ambiguous rather than guessed.
 *
 * Kept as pure functions so the matching rules are testable without a device,
 * a notification service, or a real SMS.
 */
object OtpWatcher {

    const val MIN_CODE_LENGTH = 4
    const val MAX_CODE_LENGTH = 8

    /** Text beyond this is not read: a verification message is short. */
    private const val MAX_TEXT_BYTES = 32 * 1024

    /** How far (in characters) a cue may sit from the candidate. */
    private const val CUE_WINDOW_CHARS = 96

    /**
     * Wording a verification message carries. The bare word "code" is not a
     * cue: a promo code, a zip code and an order code all carry it.
     */
    private val CUES = listOf(
        "verification code", "verification", "verify", "one-time", "one time", "passcode",
        "security code", "login code", "sign-in code", "sign in code", "confirmation code",
        "authentication code", "access code", "otp", "2fa", "two-factor", "code is", "code:",
        "your code", "use code", "enter code",
    )

    /**
     * Wording that marks material this path must never treat as a login code:
     * recovery and setup material, resets, and the codes that are not
     * verification at all (promotions, postal and area codes, tracking).
     */
    private val REFUSED = listOf(
        "recovery code", "backup code", "secret key", "setup key", "authenticator app",
        "reset your password", "password reset", "totp", "promo", "discount", "coupon", "voucher",
        "% off", "zip code", "postal code", "postcode", "area code", "qr code", "tracking",
    )

    private val URL_MARKERS = listOf("https://", "http://", "www.")

    data class Found(val code: String, val sender: String, val postedAtMs: Long)

    /** What one message's text says, in the runtime's own vocabulary. */
    sealed class Extraction {
        data class Code(val code: String) : Extraction()
        data class Ambiguous(val candidates: Int) : Extraction()
        object None : Extraction()
    }

    /**
     * Extract the one verification code a text carries: exactly one distinct
     * candidate of an approved format next to a cue, narrowed to
     * [expectedDigits] when the challenge names a length.
     */
    fun extractCode(text: String, expectedDigits: Int? = null): Extraction {
        val stripped = stripUrls(bounded(text))
        // ASCII lowercasing only: the cues are ASCII, and Unicode lowercasing
        // changes lengths (İ, K, Ω), which would misalign every index taken
        // on the lowered copy against the original.
        val lower = asciiLowercase(stripped)
        if (REFUSED.any { lower.contains(it) }) return Extraction.None
        if (CUES.none { lower.contains(it) }) return Extraction.None
        val chars = stripped.toCharArray()
        val lowerChars = lower.toCharArray()
        val candidates = LinkedHashSet<String>()
        var index = 0
        while (index < chars.size) {
            if (!chars[index].isAsciiDigit()) {
                index += 1
                continue
            }
            val start = index
            val (digits, end) = readCodeRun(chars, start)
            index = maxOf(end, start + 1)
            val boundaryOk = (start == 0 || !chars[start - 1].isLetterOrDigit()) &&
                (end >= chars.size || !chars[end].isLetterOrDigit())
            if (!boundaryOk || digits.length < MIN_CODE_LENGTH || digits.length > MAX_CODE_LENGTH) continue
            if (expectedDigits != null && digits.length != expectedDigits) continue
            if (looksLikeADateOrAmount(chars, start, end) || partOfANumberSequence(chars, start, end) || implausible(digits)) continue
            if (!cueNearby(lowerChars, start, end)) continue
            candidates.add(digits)
        }
        return when (candidates.size) {
            0 -> Extraction.None
            1 -> Extraction.Code(candidates.first())
            else -> Extraction.Ambiguous(candidates.size)
        }
    }

    /**
     * Pull a code out of one notification, or return null.
     *
     * `title` and `text` are searched together because senders split the phrase
     * across them — "Google" / "G-482913 is your verification code" being the
     * common shape.
     */
    fun extract(title: String?, text: String?, packageName: String, postedAtMs: Long): Found? {
        val body = listOfNotNull(title, text).joinToString("\n").trim()
        if (body.isEmpty()) return null
        return when (val extraction = extractCode(body)) {
            is Extraction.Code -> Found(extraction.code, packageName, postedAtMs)
            else -> null
        }
    }

    /**
     * Find the newest code among notifications posted after [since].
     *
     * Anchored on time because a stale code from ten minutes ago is worse than
     * none: it is expired, it will be rejected, and the caller will believe the
     * flow failed for some other reason.
     */
    fun newest(
        notifications: List<NotificationInfo>,
        since: Long,
    ): Found? = notifications
        .asSequence()
        .filter { it.postTime > since }
        .sortedByDescending { it.postTime }
        .mapNotNull { extract(it.title, it.text, it.packageName, it.postTime) }
        .firstOrNull()

    /** What the eligible notifications say, for a challenge (secure HITL P6). */
    sealed class Decision {
        /** Exactly one distinct code inside the window. */
        data class Code(val found: Found) : Decision()
        /** Two different codes inside the window, or one message that carries two: the person decides, not the newest. */
        data class Ambiguous(val candidates: Int) : Decision()
        object None : Decision()
    }

    /**
     * Decide for one challenge: every notification posted inside
     * `[windowStartMs, deadlineMs]` that carries a code of the expected length
     * counts; one distinct code wins, two hand the decision to the person —
     * choosing the newest between two live challenges is how the wrong code
     * gets typed.
     */
    fun decide(
        notifications: List<NotificationInfo>,
        windowStartMs: Long,
        deadlineMs: Long?,
        expectedDigits: Int?,
    ): Decision {
        val found = mutableListOf<Found>()
        var ambiguousCandidates = 0
        notifications
            .filter { it.postTime >= windowStartMs && (deadlineMs == null || it.postTime <= deadlineMs) }
            .forEach { notification ->
                val body = listOfNotNull(notification.title, notification.text).joinToString("\n").trim()
                if (body.isEmpty()) return@forEach
                when (val extraction = extractCode(body, expectedDigits)) {
                    is Extraction.Code -> found.add(Found(extraction.code, notification.packageName, notification.postTime))
                    is Extraction.Ambiguous -> ambiguousCandidates = maxOf(ambiguousCandidates, extraction.candidates)
                    Extraction.None -> {}
                }
            }
        val distinct = found.map { it.code }.distinct()
        if (ambiguousCandidates > 0) return Decision.Ambiguous(maxOf(ambiguousCandidates, distinct.size))
        return when (distinct.size) {
            0 -> Decision.None
            1 -> Decision.Code(found.maxByOrNull { it.postedAtMs } ?: found.first())
            else -> Decision.Ambiguous(distinct.size)
        }
    }

    /**
     * One run of digits from [start], or equal-sized groups joined by a single
     * space, dash, dot or non-breaking space (`123 456`, `12-34-56`) — never a
     * sentence's stray numbers (`1234 on 5`). Returns the digits and the index
     * after the run.
     */
    private fun readCodeRun(chars: CharArray, start: Int): Pair<String, Int> {
        val digits = StringBuilder()
        var cursor = start
        val firstGroup = digitRunLength(chars, start)
        while (true) {
            val group = digitRunLength(chars, cursor)
            digits.append(chars, cursor, group)
            cursor += group
            val separatorJoins = cursor + 1 < chars.size &&
                (chars[cursor] == ' ' || chars[cursor] == '-' || chars[cursor] == '.' || chars[cursor] == '\u00A0') &&
                chars[cursor + 1].isAsciiDigit() &&
                group == firstGroup &&
                digitRunLength(chars, cursor + 1) == firstGroup &&
                firstGroup <= 4
            if (separatorJoins) {
                cursor += 1
            } else {
                return digits.toString() to cursor
            }
        }
    }

    private fun digitRunLength(chars: CharArray, start: Int): Int {
        var count = 0
        var index = minOf(start, chars.size)
        while (index < chars.size && chars[index].isAsciiDigit()) {
            count += 1
            index += 1
        }
        return count
    }

    /**
     * A four-digit number that reads as a year (1900–2099), or digits that are
     * all the same — a placeholder far more often than a code. A wrong code
     * typed into a form burns an attempt; refusing these hands the rare real
     * one to the person.
     */
    private fun implausible(digits: String): Boolean {
        if (digits.length == 4) {
            val year = digits.toIntOrNull()
            if (year != null && year in 1900..2099) return true
        }
        return digits.toSet().size == 1
    }

    /** `12/09/2026`, `$1234.56`, `#12345`, `+1 555`: numbers that are not codes even when a cue is near. */
    private fun looksLikeADateOrAmount(chars: CharArray, start: Int, end: Int): Boolean {
        val before = if (start >= 1) chars[start - 1] else null
        val after = if (end < chars.size) chars[end] else null
        return before in setOf('$', '€', '£', '₹', '+', '/', ':', '#') || after in setOf('/', ':', '%')
    }

    /**
     * A digit group standing one separator from another digit group of a
     * different size — a phone number (`+1 555 0100`, `555-0100`), an order id
     * (`12-3456`), a thousands-separated amount (`1.234.567`) — is not a code;
     * equal groups were already joined by [readCodeRun], so a real grouped code
     * (`482-913`) never reaches here.
     *
     * The separator set matches the ones [readCodeRun] declines to join across.
     * Knowing only about a space read `Verify your order 12-3456` as the code
     * `3456` — a single candidate, so no ambiguity fallback, and a notification
     * carries no sender to check.
     */
    private fun partOfANumberSequence(chars: CharArray, start: Int, end: Int): Boolean {
        val separators = setOf(' ', '-', '.', '\u00A0')
        val previousIsDigit =
            start >= 2 && chars[start - 1] in separators && chars[start - 2].isAsciiDigit()
        val nextIsDigit =
            end + 1 < chars.size && chars[end] in separators && chars[end + 1].isAsciiDigit()
        return previousIsDigit || nextIsDigit
    }

    /** A cue within [CUE_WINDOW_CHARS] characters on either side of the candidate. */
    private fun cueNearby(lower: CharArray, start: Int, end: Int): Boolean {
        val from = maxOf(0, start - CUE_WINDOW_CHARS)
        val to = minOf(lower.size, end + CUE_WINDOW_CHARS)
        val window = String(lower, from, to - from)
        return CUES.any { window.contains(it) }
    }

    /** Remove URLs so a link's digits never read as a code and a link is never followed. */
    fun stripUrls(text: String): String {
        val out = StringBuilder(text.length)
        var rest = text
        while (true) {
            // `ignoreCase` compares in place, so the index is the original's.
            val position = URL_MARKERS.map { rest.indexOf(it, ignoreCase = true) }.filter { it >= 0 }.minOrNull() ?: break
            out.append(rest, 0, position).append(' ')
            val tail = rest.substring(position)
            val end = tail.indexOfFirst { it.isWhitespace() || it == '<' || it == '>' || it == '"' || it == '\'' }
            rest = if (end < 0) "" else tail.substring(end)
        }
        out.append(rest)
        return out.toString()
    }

    private fun bounded(text: String): String {
        val out = StringBuilder()
        var bytes = 0
        for (ch in text) {
            val width = ch.toString().toByteArray(Charsets.UTF_8).size
            if (bytes + width > MAX_TEXT_BYTES) break
            out.append(ch)
            bytes += width
        }
        return out.toString()
    }

    private fun asciiLowercase(text: String): String =
        String(CharArray(text.length) { index ->
            val ch = text[index]
            if (ch in 'A'..'Z') ch + ('a' - 'A') else ch
        })

    private fun Char.isAsciiDigit(): Boolean = this in '0'..'9'
}
