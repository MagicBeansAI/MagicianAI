package ai.magicbeans.magdroid.attention

import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.access.MagicianConnectionSnapshot
import android.content.Context
import io.ktor.client.HttpClient
import io.ktor.client.engine.cio.CIO
import io.ktor.client.plugins.HttpTimeout
import io.ktor.client.request.header
import io.ktor.client.request.post
import io.ktor.client.request.setBody
import io.ktor.client.statement.bodyAsText
import io.ktor.http.ContentType
import io.ktor.http.contentType
import io.ktor.http.isSuccess
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.*
import java.util.concurrent.atomic.AtomicBoolean

class BrowserSignInPageChanged : Exception("The page changed. Review the picture before trying again.")

/** Settings → Service accounts row status; engines before it omit the field (`UNKNOWN`). */
enum class ServiceAccountStatus(val wire: String, val title: String) {
    CONNECTED("connected", "Connected"),
    NEEDS_SIGN_IN("needs_sign_in", "Needs sign-in"),
    AUTHENTICATING("authenticating", "Signing in…"),
    UNKNOWN("unknown", "Unknown"),
    /** Missing binary or environment; the row's detail says what. No Connect. */
    UNAVAILABLE("unavailable", "Unavailable");

    companion object {
        fun fromWire(value: String?): ServiceAccountStatus = entries.firstOrNull { it.wire == value } ?: UNKNOWN
    }
}

/** How a managed bot (Google Workspace, Telegram, WhatsApp) can be connected. */
enum class ManagedBotMethod(val wire: String) {
    LOGIN_URL("login_url"),
    CODE("code"),
    PAIRING_CODE("pairing_code"),
    QR("qr");

    /** Code and pairing-code logins start from the account's phone number. */
    val needsPhone: Boolean get() = this == CODE || this == PAIRING_CODE

    fun label(bot: String): String = when (this) {
        LOGIN_URL -> "Sign in with Google"
        CODE -> "Code"
        PAIRING_CODE -> "Link with phone number"
        QR -> if (bot.startsWith("telegram")) "QR" else "Scan QR"
    }

    companion object {
        /** Known methods in the row's order; a row without them falls back by bot name. */
        fun parseList(value: JsonElement?, bot: String?): List<ManagedBotMethod> {
            val listed = (value as? JsonArray).orEmpty()
                .mapNotNull { item -> (item as? JsonPrimitive)?.takeIf { it.isString }?.content?.let { wire -> entries.firstOrNull { it.wire == wire } } }
                .distinct()
            if (listed.isNotEmpty()) return listed
            val name = bot.orEmpty()
            return when {
                name.startsWith("gmail-") -> listOf(LOGIN_URL)
                name.startsWith("telegram") -> listOf(CODE, QR)
                name.startsWith("whatsapp") -> listOf(PAIRING_CODE, QR)
                else -> emptyList()
            }
        }
    }
}

/** One Service-accounts row. A multi-endpoint skill lists one row per declared endpoint alias. */
data class ServiceAccount(
    val name: String,
    val label: String,
    val status: ServiceAccountStatus = ServiceAccountStatus.UNKNOWN,
    val detail: String? = null,
    val endpointAlias: String? = null,
    /** `governed_oauth` | `browser_cli` | `managed_bot`; null from engines before managed bots. */
    val kind: String? = null,
    val bot: String? = null,
    val profile: String? = null,
    val methods: List<ManagedBotMethod> = emptyList(),
) {
    val key: String get() = (bot ?: endpointAlias)?.let { "$name:$it" } ?: name

    /** The bot to connect through `bot/start`, for a managed-bot row. */
    val managedBot: String? get() = bot.takeIf { kind == "managed_bot" }

    companion object {
        /** Backward compatible: `status`, `detail`, `endpoint_alias`, `kind`, `bot`, `profile` and `methods` are optional. */
        fun parseList(result: JsonObject): List<ServiceAccount> =
            (result["accounts"] as? JsonArray).orEmpty().mapNotNull { value ->
                val row = value as? JsonObject ?: return@mapNotNull null
                fun text(key: String) = (row[key] as? JsonPrimitive)?.takeIf { it.isString }?.content?.takeIf { it.isNotEmpty() }
                val name = text("name") ?: return@mapNotNull null
                val kind = text("kind")
                val bot = text("bot")
                ServiceAccount(
                    name = name,
                    label = text("label") ?: name,
                    status = ServiceAccountStatus.fromWire(text("status")),
                    detail = text("detail"),
                    endpointAlias = text("endpoint_alias"),
                    kind = kind,
                    bot = bot,
                    profile = text("profile"),
                    methods = if (kind == "managed_bot") ManagedBotMethod.parseList(row["methods"], bot) else emptyList(),
                )
            }
    }
}

/** `BotConnectState` from `bot/start|input|state|cancel`. */
data class BotConnectState(
    val bot: String,
    val status: ServiceAccountStatus,
    val detail: String? = null,
    val pending: Pending? = null,
) {
    enum class PendingKind(val wire: String) { PHONE("phone"), CODE("code"), PASSWORD("password"), PAIRING_CODE("pairing_code"), LOGIN_URL("login_url"), QR("qr") }

    data class Pending(
        val kind: PendingKind,
        val label: String? = null,
        val pairingCode: String? = null,
        /** Base64 PNG bytes from a `data:image/png;base64,…` URL (the engine sends PNG QR codes only). */
        val qrPngBase64: String? = null,
        val signInMode: String? = null,
        val signInTicket: String? = null,
    )

    companion object {
        private const val PNG_PREFIX = "data:image/png;base64,"

        fun parse(bot: String, result: JsonObject): BotConnectState {
            fun JsonObject.text(key: String) = (this[key] as? JsonPrimitive)?.takeIf { it.isString }?.content?.takeIf { it.isNotEmpty() }
            val raw = result["pending"] as? JsonObject
            val kind = raw?.text("kind")?.let { wire -> PendingKind.entries.firstOrNull { it.wire == wire } }
            val pending = if (raw != null && kind != null) {
                val signIn = raw["sign_in"] as? JsonObject
                Pending(
                    kind = kind,
                    label = raw.text("label"),
                    pairingCode = raw.text("pairing_code"),
                    qrPngBase64 = raw.text("qr_data_url")?.takeIf { it.startsWith(PNG_PREFIX) }?.removePrefix(PNG_PREFIX),
                    signInMode = signIn?.text("mode"),
                    signInTicket = signIn?.text("ticket"),
                )
            } else null
            return BotConnectState(
                bot = result.text("bot") ?: bot,
                status = ServiceAccountStatus.fromWire(result.text("status")),
                detail = result.text("detail"),
                pending = pending,
            )
        }
    }
}

/** Owner credentials never enter the streamed page or a model tool call. */
class BrowserSignInClient(context: Context, expected: MagicianConnectionSnapshot? = null) : AutoCloseable {
    private val app = context.applicationContext
    private val connection = MagicianAccess.connectionSnapshot(app)
        ?: throw AttentionError("Connect to your Magican computer first.")
    private val client = HttpClient(CIO) { followRedirects = false; install(HttpTimeout) { requestTimeoutMillis = 45_000; connectTimeoutMillis = 15_000 } }
    private var credential = ""
    val originConnection: MagicianConnectionSnapshot get() = connection
    private val stopped = AtomicBoolean(false)
    init { if (expected != null && expected != connection) { client.close(); throw AttentionError("Connection changed. Reopen sign-in.") } }

    private fun current() {
        if (connection != MagicianAccess.connectionSnapshot(app)) throw AttentionError("Connection changed. Reopen sign-in on the intended computer.")
    }

    private suspend fun request(action: String, body: JsonObject = buildJsonObject {}, capability: String = credential): JsonObject {
        if (action != "close") { current(); check(!stopped.get()) { "Sign-in is closed." } }
        val response = client.post("${connection.baseUrl.trimEnd('/')}/api/magician/v2/auth/browser-signin/$action") {
            connection.headers.forEach { (name, value) -> header(name, value) }
            if (capability.isNotEmpty()) header("X-Magician-Signin", capability)
            header("Cache-Control", "no-store")
            contentType(ContentType.Application.Json)
            setBody(body.toString())
        }
        val text = response.bodyAsText()
        if (action != "close") { current(); check(!stopped.get()) { "Sign-in is closed." } }
        if (text.length > 4 * 1024 * 1024) throw AttentionError("Sign-in frame is too large.")
        val result = attentionJson.parseToJsonElement(text).jsonObject
        if (!response.status.isSuccess()) {
            if (result["error"]?.jsonPrimitive?.content == "signin_page_changed") throw BrowserSignInPageChanged()
            // Launch and managed-bot errors carry a short, secret-free message.
            val message = (result["message"] as? JsonPrimitive)?.takeIf { (action == "launch" || action.startsWith("bot/")) && it.isString }?.content?.takeIf { it.isNotEmpty() }?.take(1024)
            throw AttentionError(message ?: "Sign-in could not continue. Reopen the request, or finish on the computer.")
        }
        return result
    }

    suspend fun accounts(): JsonObject = request("accounts")

    /** Managed-bot connect (`bot/start|input|state|cancel`), owner headers only. */
    /** [openOnComputer] opens Google's sign-in in the computer's own browser (`sign_in.mode = computer_opened`). */
    suspend fun botStart(bot: String, method: ManagedBotMethod, phone: String? = null, openOnComputer: Boolean = false): BotConnectState =
        BotConnectState.parse(bot, request("bot/start", buildJsonObject {
            put("bot", bot); put("method", method.wire); if (phone != null) put("phone", phone)
            if (openOnComputer) put("open_on_computer", true)
        }, capability = ""))

    /** `step`: `phone` | `code` | `password`. The value goes to the login process only. */
    suspend fun botInput(bot: String, step: String, value: String): BotConnectState =
        BotConnectState.parse(bot, request("bot/input", buildJsonObject { put("bot", bot); put("step", step); put("value", value) }, capability = ""))

    suspend fun botState(bot: String): BotConnectState =
        BotConnectState.parse(bot, request("bot/state", buildJsonObject { put("bot", bot) }, capability = ""))

    suspend fun botCancel(bot: String): BotConnectState =
        BotConnectState.parse(bot, request("bot/cancel", buildJsonObject { put("bot", bot) }, capability = ""))

    suspend fun launch(correlationId: String? = null, skill: String? = null, openOnComputer: Boolean = false, endpointAlias: String? = null): JsonObject = request("launch", buildJsonObject {
        if (skill != null) put("skill", skill) else put("correlation_id", correlationId.orEmpty())
        if (skill != null && endpointAlias != null) put("endpoint_alias", endpointAlias)
        if (openOnComputer) put("open_on_computer", true)
    })

    suspend fun claim(ticket: String): Boolean {
        check(!stopped.get()) { "Sign-in is closed." }
        if (!ticket.matches(Regex("bsi_[a-f0-9]{64}"))) throw AttentionError("Invalid sign-in response.")
        credential = ticket
        val origin = java.net.URI(connection.baseUrl)
        if (origin.scheme != "https" && !(origin.scheme == "http" && origin.host in setOf("localhost", "127.0.0.1", "[::1]", "::1"))) {
            throw AttentionError("Use HTTPS to your Magican computer for private sign-in, or choose Open on computer.")
        }
        val result = request("claim")
        if (result["status"]?.jsonPrimitive?.content == "connected") { credential = ""; return false }
        credential = result["session_token"]?.jsonPrimitive?.content
            ?.takeIf { it.matches(Regex("bsi_[a-f0-9]{64}")) } ?: throw AttentionError("Sign-in could not start.")
        return true
    }

    suspend fun frame(): JsonObject = request("frame")
    suspend fun input(sequence: Long, revision: String, value: JsonObject) = request("input", buildJsonObject {
        put("sequence", sequence); put("frame_revision", revision); put("input", value)
    })

    override fun close() {
        if (stopped.getAndSet(true)) return
        val token = credential
        credential = ""
        if (token.isEmpty()) { client.close(); return }
        cleanup.launch {
            try { withTimeout(5000) { runCatching { request("close", capability = token) } } }
            finally { client.close() }
        }
    }
    companion object { private val cleanup = CoroutineScope(SupervisorJob() + Dispatchers.IO) }
}
