package ai.magicbeans.magdroid.chat

/**
 * What kind of thing a task produced.
 *
 * A port of `ArtifactKind` in `ArtifactViewer.swift`, extension fallback
 * included. The mime type was decoded here and never read, so every output —
 * an image, a PDF, a recording, a chart — rendered as the same generic
 * document row, and the list said nothing about what was in it.
 *
 * The extension is consulted because the mime is frequently absent or
 * `application/octet-stream`: a file called `chart.png` is an image whatever
 * the header claims, and guessing from the name is what the other clients do
 * rather than showing a shrug.
 */
enum class ArtifactKind(val label: String) {
    Image("Image"),
    Pdf("PDF document"),
    Html("HTML page"),
    Video("Video"),
    Audio("Audio"),
    Markdown("Markdown"),
    Json("JSON"),
    Text("Text"),
    Other("File"),
    ;

    companion object {
        fun from(mime: String?, filename: String?): ArtifactKind {
            val m = mime.orEmpty().trim().lowercase()
            val ext = filename.orEmpty().substringAfterLast('.', "").lowercase()

            return when {
                m.startsWith("image/") || ext in IMAGE -> Image
                m == "application/pdf" || ext == "pdf" -> Pdf
                m == "text/html" || m == "application/xhtml+xml" || ext in HTML -> Html
                m.startsWith("video/") || ext in VIDEO -> Video
                m.startsWith("audio/") || ext in AUDIO -> Audio
                m == "text/markdown" || ext in MARKDOWN -> Markdown
                m == "application/json" || m == "text/json" || ext == "json" -> Json
                m.startsWith("text/") || ext in TEXT -> Text
                else -> Other
            }
        }

        private val IMAGE = setOf("png", "jpg", "jpeg", "gif", "webp", "heic")
        private val HTML = setOf("html", "htm", "xhtml")
        private val VIDEO = setOf("mp4", "mov", "m4v", "webm")
        private val AUDIO = setOf("mp3", "wav", "m4a", "aac")
        private val MARKDOWN = setOf("md", "markdown")
        private val TEXT = setOf("txt", "log", "csv", "tsv")
    }
}
