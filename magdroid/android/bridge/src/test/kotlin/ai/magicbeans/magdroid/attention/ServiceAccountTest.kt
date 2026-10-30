package ai.magicbeans.magdroid.attention

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Settings → Service accounts rows from engines before and after per-row status. */
class ServiceAccountTest {
    private fun parse(json: String) = ServiceAccount.parseList(Json.parseToJsonElement(json).jsonObject)

    @Test
    fun decodesStatusDetailAndEndpointAlias() {
        val rows = parse(
            """{"accounts":[
                {"name":"metabase-mcp","label":"Provider account","status":"connected","detail":"Account login is active"},
                {"name":"swiggy-mcp","label":"Provider account — instamart","status":"needs_sign_in","detail":"Account login is required","endpoint_alias":"instamart"}
            ]}""",
        )
        assertEquals(2, rows.size)
        assertEquals(ServiceAccountStatus.CONNECTED, rows[0].status)
        assertEquals("Account login is active", rows[0].detail)
        assertNull(rows[0].endpointAlias)
        assertEquals("metabase-mcp", rows[0].key)
        assertEquals(ServiceAccountStatus.NEEDS_SIGN_IN, rows[1].status)
        assertEquals("instamart", rows[1].endpointAlias)
        assertEquals("swiggy-mcp:instamart", rows[1].key)
    }

    @Test
    fun olderEngineRowsReadAsUnknown() {
        val rows = parse("""{"accounts":[{"name":"higgsfield","label":"Higgsfield account"},{"label":"no name"},"junk"]}""")
        assertEquals(1, rows.size)
        assertEquals(ServiceAccountStatus.UNKNOWN, rows[0].status)
        assertEquals("Unknown", rows[0].status.title)
        assertNull(rows[0].detail)
    }

    @Test
    fun unknownStatusValuesFallBackToUnknown() {
        val rows = parse("""{"accounts":[{"name":"x","label":"x","status":"from_the_future"}]}""")
        assertEquals(ServiceAccountStatus.UNKNOWN, rows.single().status)
    }

    @Test
    fun decodesManagedBotRowsAndUnavailable() {
        val rows = parse(
            """{"accounts":[
                {"name":"google-workspace","label":"Google Workspace (work)","kind":"managed_bot","bot":"gmail-work","profile":"work","methods":["login_url"],"status":"connected"},
                {"name":"google-workspace","label":"Google Workspace (business)","kind":"managed_bot","bot":"gmail-business","profile":"business","methods":["login_url"],"status":"needs_sign_in"},
                {"name":"whatsapp","label":"WhatsApp","kind":"managed_bot","bot":"whatsapp","methods":["pairing_code","qr","fax"],"status":"authenticating"},
                {"name":"telegram","label":"Telegram","kind":"managed_bot","bot":"telegram-self","methods":["code","qr"],"status":"unavailable","detail":"Set TGCLI_API_ID and TGCLI_API_HASH"},
                {"name":"metabase-mcp","label":"Provider account","kind":"governed_oauth","status":"connected"}
            ]}""",
        )
        assertEquals(5, rows.size)
        assertEquals("gmail-work", rows[0].managedBot)
        assertEquals("work", rows[0].profile)
        assertEquals(listOf(ManagedBotMethod.LOGIN_URL), rows[0].methods)
        assertNotEquals("two Google profiles of one skill are separate rows", rows[0].key, rows[1].key)
        assertEquals(listOf(ManagedBotMethod.PAIRING_CODE, ManagedBotMethod.QR), rows[2].methods)
        assertTrue(rows[2].methods[0].needsPhone)
        assertEquals("Link with phone number", rows[2].methods[0].label("whatsapp"))
        assertEquals(ServiceAccountStatus.UNAVAILABLE, rows[3].status)
        assertEquals("Unavailable", rows[3].status.title)
        assertEquals("Set TGCLI_API_ID and TGCLI_API_HASH", rows[3].detail)
        assertEquals(listOf("Code", "QR"), rows[3].methods.map { it.label("telegram-self") })
        assertEquals("governed_oauth", rows[4].kind)
        assertNull(rows[4].managedBot)
        assertEquals(emptyList<ManagedBotMethod>(), rows[4].methods)
    }

    @Test
    fun managedBotMethodsFallBackByBotName() {
        val rows = parse(
            """{"accounts":[
                {"name":"telegram","label":"Telegram","kind":"managed_bot","bot":"telegram-self"},
                {"name":"google-workspace","label":"Google","kind":"managed_bot","bot":"gmail-work","methods":[]}
            ]}""",
        )
        assertEquals(listOf(ManagedBotMethod.CODE, ManagedBotMethod.QR), rows[0].methods)
        assertEquals(listOf(ManagedBotMethod.LOGIN_URL), rows[1].methods)
    }

    private fun state(bot: String, json: String) = BotConnectState.parse(bot, Json.parseToJsonElement(json).jsonObject)

    @Test
    fun decodesBotConnectState() {
        val code = state(
            "telegram-self",
            """{"bot":"telegram-self","status":"authenticating","detail":"Enter the code Telegram sent you",
                "pending":{"kind":"code","label":"Telegram code","pairing_code":null,"qr_data_url":null,"sign_in":null}}""",
        )
        assertEquals(ServiceAccountStatus.AUTHENTICATING, code.status)
        assertEquals("Enter the code Telegram sent you", code.detail)
        assertEquals(BotConnectState.PendingKind.CODE, code.pending?.kind)
        assertEquals("Telegram code", code.pending?.label)
        assertNull(code.pending?.pairingCode)

        val pairing = state("whatsapp", """{"status":"authenticating","pending":{"kind":"pairing_code","pairing_code":"ABCD1234"}}""")
        assertEquals("whatsapp", pairing.bot)
        assertEquals("ABCD1234", pairing.pending?.pairingCode)

        val qr = state("whatsapp", """{"status":"authenticating","pending":{"kind":"qr","qr_data_url":"data:image/png;base64,iVBORw0KGgo="}}""")
        assertEquals("iVBORw0KGgo=", qr.pending?.qrPngBase64)
        val svg = state("whatsapp", """{"status":"authenticating","pending":{"kind":"qr","qr_data_url":"data:image/svg+xml;base64,PHN2Zy8+"}}""")
        assertNull(svg.pending?.qrPngBase64)

        val ticket = "bsi_" + "c".repeat(64)
        val google = state(
            "gmail-work",
            """{"status":"authenticating","pending":{"kind":"login_url","sign_in":{"mode":"computer","ticket":"$ticket","viewer_path":"/api/magician/v2/auth/browser-signin/view","expires_in":300}}}""",
        )
        assertEquals(BotConnectState.PendingKind.LOGIN_URL, google.pending?.kind)
        assertEquals("computer", google.pending?.signInMode)
        assertEquals(ticket, google.pending?.signInTicket)

        val idle = state("gmail-work", """{"status":"from_the_future","pending":{"kind":"nope"}}""")
        assertEquals(ServiceAccountStatus.UNKNOWN, idle.status)
        assertNull(idle.pending)
    }
}
