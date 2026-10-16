package ai.magicbeans.magdroid.access

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class MachineProfilesTest {
    private val macbook = MachineProfile(
        baseUrl = "https://macbookpro.example.com",
        cfAccessClientId = "mbp-id",
        cfAccessClientSecret = "mbp-secret",
        principal = "owner",
        workspace = "private",
        bridgePairingToken = "mbp-token",
        deviceId = "magdroid-1",
    )
    private val studio = MachineProfile(
        baseUrl = "https://macstudio.example.com",
        cfAccessClientId = "studio-id",
        cfAccessClientSecret = "studio-secret",
        principal = "owner",
        workspace = "private",
        bridgePairingToken = "studio-token",
        deviceId = "magdroid-1",
    )

    @Test fun `same origin re-pair scope and credential changes invalidate active work`() {
        val before = MachineProfileBook().upsert(macbook)
        listOf(
            macbook.copy(bridgePairingToken = "rotated-token"),
            macbook.copy(principal = "another-owner"),
            macbook.copy(workspace = "another-workspace"),
            macbook.copy(cfAccessClientSecret = "rotated-access-secret"),
            macbook.copy(deviceId = "new-device"),
        ).forEach { changed ->
            assertTrue(before.upsert(changed).activeConnectionChangedFrom(before))
        }
        assertFalse(before.upsert(macbook.copy(machineName = "Renamed machine")).activeConnectionChangedFrom(before))
        val inactiveChange = before.upsert(studio).activate(macbook.origin)
        assertFalse(inactiveChange.activeConnectionChangedFrom(before))
    }

    @Test fun `v1 flat keys migrate into the only, active profile`() {
        val v1 = mapOf(
            "base_url" to "https://MacBookPro.example.com/",
            "cf_access_client_id" to "mbp-id",
            "cf_access_client_secret" to "mbp-secret",
            "principal" to "owner",
            "workspace" to "private",
            "bridge_pairing_token" to "mbp-token",
            "device_id" to "magdroid-1",
            // Install-wide keys are not part of a profile.
            "apps_automation_key_alias" to "alias",
            "pending_apps_base_url" to "https://macbookpro.example.com",
        )
        val book = MachineProfileBook.migrateFromV1(v1)

        assertEquals("https://macbookpro.example.com", book.activeOrigin)
        assertEquals(setOf("https://macbookpro.example.com"), book.profiles.keys)
        assertEquals(macbook, book.active)
    }

    @Test fun `a v1 install that never paired migrates to an empty book`() {
        val book = MachineProfileBook.migrateFromV1(mapOf("device_id" to "magdroid-1"))
        assertTrue(book.isEmpty())
        assertNull(book.activeOrigin)
        assertNull(book.active)
    }

    @Test fun `migrated book survives an encode and decode round trip`() {
        val book = MachineProfileBook().upsert(macbook).upsert(studio)
        val decoded = MachineProfileBook.decode(book.activeOrigin, book.encodeProfiles())
        assertEquals(book, decoded)
        assertEquals(listOf(macbook.origin, studio.origin), decoded.profiles.keys.toList())
    }

    @Test fun `corrupt or dangling stored state decodes safely`() {
        assertTrue(MachineProfileBook.decode("https://x.example", "{not json").isEmpty())
        val book = MachineProfileBook().upsert(macbook)
        val dangling = MachineProfileBook.decode("https://gone.example", book.encodeProfiles())
        assertNull(dangling.activeOrigin)
        assertEquals(1, dangling.profiles.size)
    }

    @Test fun `saving an enrollment keeps other machines and makes the new one active`() {
        val book = MachineProfileBook().upsert(macbook).upsert(studio)

        assertEquals(studio.origin, book.activeOrigin)
        assertEquals(setOf(macbook.origin, studio.origin), book.profiles.keys)
        assertEquals("mbp-token", book.profiles.getValue(macbook.origin).bridgePairingToken)

        // Re-pairing a machine replaces only that machine's credential.
        val repaired = book.upsert(macbook.copy(bridgePairingToken = "mbp-token-2"))
        assertEquals(macbook.origin, repaired.activeOrigin)
        assertEquals("mbp-token-2", repaired.active?.bridgePairingToken)
        assertEquals("studio-token", repaired.profiles.getValue(studio.origin).bridgePairingToken)
        assertEquals(2, repaired.profiles.size)
    }

    @Test fun `activate switches the active base url and credentials`() {
        val book = MachineProfileBook().upsert(macbook).upsert(studio)
        val switched = book.activate("https://MACBOOKPRO.example.com/")

        assertEquals("https://macbookpro.example.com", switched.active?.baseUrl)
        assertEquals("mbp-token", switched.active?.bridgePairingToken)
        assertEquals("mbp-secret", switched.active?.cfAccessClientSecret)
        assertEquals(book.profiles, switched.profiles)
        assertEquals(
            listOf(true, false),
            switched.pairedMachines().map { it.active },
        )
    }

    @Test fun `activate refuses an origin this phone never paired`() {
        val book = MachineProfileBook().upsert(macbook)
        listOf(
            "https://macstudio.example.com",
            "https://macbookpro.example.com.evil.test",
            "http://macbookpro.example.com",
            "https://macbookpro.example.com:8443",
            "https://user@macbookpro.example.com",
            "",
            "not a url",
        ).forEach { candidate ->
            assertThrows(IllegalArgumentException::class.java) { book.activate(candidate) }
        }
        assertThrows(IllegalArgumentException::class.java) {
            MachineProfileBook().activate(macbook.baseUrl)
        }
    }

    @Test fun `forget removes one machine and falls back to another`() {
        val book = MachineProfileBook().upsert(macbook).upsert(studio)

        val forgotInactive = book.forget(macbook.origin)
        assertEquals(setOf(studio.origin), forgotInactive.profiles.keys)
        assertEquals(studio.origin, forgotInactive.activeOrigin)

        val forgotActive = book.forget(studio.origin)
        assertEquals(setOf(macbook.origin), forgotActive.profiles.keys)
        assertEquals(macbook.origin, forgotActive.activeOrigin)

        val none = forgotActive.forget(macbook.origin)
        assertTrue(none.isEmpty())
        assertNull(none.activeOrigin)

        // A forgotten origin is a never-paired origin again.
        assertThrows(IllegalArgumentException::class.java) { forgotActive.forget(macbook.origin).activate(macbook.origin) }
        assertEquals(book, book.forget("https://unknown.example"))
    }

    @Test fun `clearing signs out of the active machine only and activates nothing`() {
        val book = MachineProfileBook().upsert(macbook).upsert(studio)
        val cleared = book.clearActive()
        assertNull(cleared.activeOrigin)
        assertEquals(setOf(macbook.origin), cleared.profiles.keys)
        assertEquals(cleared, cleared.clearActive())
    }

    @Test fun `updateActive touches only the active profile and keeps its origin`() {
        val book = MachineProfileBook().upsert(macbook).upsert(studio)
        val updated = book.updateActive { it.copy(cfAccessClientId = "rotated", baseUrl = "https://evil.example") }
        assertEquals("rotated", updated.active?.cfAccessClientId)
        assertEquals(studio.origin, updated.active?.baseUrl)
        assertEquals(macbook, updated.profiles.getValue(macbook.origin))
        assertEquals(MachineProfileBook(), MachineProfileBook().updateActive { it.copy(cfAccessClientId = "x") })
    }

    @Test fun `diagnostic strings never carry credentials`() {
        val book = MachineProfileBook().upsert(macbook)
        listOf(macbook.toString(), book.toString()).forEach { text ->
            assertFalse(text.contains("mbp-secret"))
            assertFalse(text.contains("mbp-token"))
        }
    }

    private val macbookLan = macbook.copy(
        baseUrl = "http://192.168.1.20:3002",
        bridgePairingToken = "mbp-lan-token",
    )

    @Test fun `machine origins naming an existing lan profile replace it`() {
        val book = MachineProfileBook().upsert(macbookLan).upsert(studio)

        val repaired = book.upsertReplacingSameMachine(
            macbook,
            listOf("https://MacBookPro.example.com/", "http://192.168.1.20:3002/"),
        )

        assertEquals(setOf(studio.origin, macbook.origin), repaired.profiles.keys)
        assertEquals(macbook.origin, repaired.activeOrigin)
        assertEquals("mbp-token", repaired.active?.bridgePairingToken)
        assertEquals(studio, repaired.profiles.getValue(studio.origin))
    }

    @Test fun `the same machine name replaces the other route's profile`() {
        val book = MachineProfileBook().upsert(macbook.copy(machineName = "macbookpro"))

        val repaired = book.upsertReplacingSameMachine(macbookLan.copy(machineName = " MacBookPro "))

        assertEquals(setOf("http://192.168.1.20:3002"), repaired.profiles.keys)
        assertEquals("http://192.168.1.20:3002", repaired.activeOrigin)
        assertEquals("mbp-lan-token", repaired.active?.bridgePairingToken)
    }

    @Test fun `a different machine is kept when pairing by another route`() {
        val book = MachineProfileBook()
            .upsert(studio.copy(machineName = "macstudio"))
            // No stored name: an older profile is never matched by name.
            .upsert(macbook)

        val repaired = book.upsertReplacingSameMachine(
            macbookLan.copy(machineName = "macbookpro"),
            listOf("https://other-name.example.com", "not a url", ""),
        )

        assertEquals(
            setOf(studio.origin, macbook.origin, macbookLan.origin),
            repaired.profiles.keys,
        )
        assertEquals(macbookLan.origin, repaired.activeOrigin)
        // Without a machine name or origins it is exactly upsert.
        assertEquals(book.upsert(macbookLan), book.upsertReplacingSameMachine(macbookLan))
        // A blank name never matches another blank name.
        val blank = MachineProfileBook().upsert(studio.copy(machineName = " "))
        assertEquals(2, blank.upsertReplacingSameMachine(macbook.copy(machineName = "")).profiles.size)
    }

    @Test fun `stored v2 json without a machine name still decodes`() {
        val old = """
            {"https://macbookpro.example.com":{"base_url":"https://macbookpro.example.com",
            "cf_access_client_id":"mbp-id","cf_access_client_secret":"mbp-secret",
            "principal":"owner","workspace":"private","bridge_pairing_token":"mbp-token",
            "device_id":"magdroid-1"}}
        """.trimIndent()
        val book = MachineProfileBook.decode("https://macbookpro.example.com", old)

        assertEquals(macbook, book.active)
        assertNull(book.active?.machineName)
        assertNull(book.pairedMachines().single().machineName)

        val named = MachineProfileBook().upsert(macbook.copy(machineName = "macbookpro"))
        val roundTrip = MachineProfileBook.decode(named.activeOrigin, named.encodeProfiles())
        assertEquals("macbookpro", roundTrip.pairedMachines().single().machineName)
    }

    @Test fun `requirePairedOrigin returns the stored key for a normalized match`() {
        val paired = listOf("https://macbookpro.example.com", "http://192.168.1.20:13002")
        assertEquals("https://macbookpro.example.com", requirePairedOrigin(paired, "https://MacBookPro.example.com/"))
        assertEquals("http://192.168.1.20:13002", requirePairedOrigin(paired, "http://192.168.1.20:13002"))
        assertThrows(IllegalArgumentException::class.java) {
            requirePairedOrigin(paired, "http://192.168.1.20:3002")
        }
    }
}
