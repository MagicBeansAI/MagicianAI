package ai.magicbeans.magdroid.chat

data class AnswerVerificationNote(val details: String?) {
    companion object {
        const val EXPLANATION = "Some parts of this answer could not be confirmed against the available sources."
    }
}

data class AnswerVerificationDisplay(val body: String, val verification: AnswerVerificationNote?)

private val verificationPrefix = Regex("^\\s*(?:>\\s*)?⚠\uFE0F?\\s*(?:\\*\\*)?Not fully verified\\.(?:\\*\\*)?[ \\t]*")
private val verificationDiagnostic = Regex("^(?:Magician|Magican)['’]s evidence check could not confirm part of this answer:[ \\t]*")
private val paragraphBreak = Regex("\\r?\\n[ \\t]*\\r?\\n")
private val escapedPunctuation = Regex("\\\\([\\\\`*{}\\[\\]()#+.!_>~-])")

/** Extract the disclosure without altering the stored answer, figures or copied content. */
fun splitAnswerVerification(text: String): AnswerVerificationDisplay {
    val prefix = verificationPrefix.find(text) ?: return AnswerVerificationDisplay(text, null)
    val rest = text.substring(prefix.range.last + 1)
    val diagnostic = verificationDiagnostic.find(rest)
        ?: return AnswerVerificationDisplay(rest.trimStart(), AnswerVerificationNote(null))
    val separator = paragraphBreak.find(rest)
    val details = rest.substring(diagnostic.range.last + 1, separator?.range?.first ?: rest.length)
        .replace(escapedPunctuation, "$1").trim().takeIf(String::isNotEmpty)
    val body = separator?.let { rest.substring(it.range.last + 1).trimStart() }.orEmpty()
    return AnswerVerificationDisplay(body, AnswerVerificationNote(details))
}

fun withoutAnswerVerification(response: StructuredResponse): StructuredResponse = response.copy(
    summary = response.summary?.let { splitAnswerVerification(it).body.takeIf(String::isNotEmpty) },
    blocks = response.blocks.mapNotNull { block ->
        if (block.kind !in setOf("markdown", "text", "callout") || block.text == null) block
        else splitAnswerVerification(block.text).let { display ->
            if (display.verification != null && display.body.isEmpty()) null else block.copy(text = display.body)
        }
    },
)
