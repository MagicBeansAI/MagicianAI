package ai.magicbeans.magdroid.access

import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Test

class AndroidAutomationIdentityTest {
    @Test fun `enrollment proof bytes bind the exact reviewed app version`() {
        val first = AndroidAutomationIdentityManager.enrollmentSigningBytes(
            enrollmentId = "enrollment-1234567890",
            deviceId = "device",
            label = "Pixel",
            keyId = "test-key-id-123456",
            publicKeySpkiBase64 = "spki",
            appPackage = "ai.magicbeans.magdroid",
            appVersionCode = 11,
            appSigningSha256 = "a".repeat(64),
            apkSha256 = "b".repeat(64),
            connectionSecretSha256 = "c".repeat(64),
            challenge = ByteArray(32) { 4 },
        )
        val other = AndroidAutomationIdentityManager.enrollmentSigningBytes(
            enrollmentId = "enrollment-1234567890",
            deviceId = "device",
            label = "Pixel",
            keyId = "test-key-id-123456",
            publicKeySpkiBase64 = "spki",
            appPackage = "ai.magicbeans.magdroid",
            appVersionCode = 10,
            appSigningSha256 = "a".repeat(64),
            apkSha256 = "b".repeat(64),
            connectionSecretSha256 = "c".repeat(64),
            challenge = ByteArray(32) { 4 },
        )
        assertNotEquals(first.toList(), other.toList())
    }

    @Test fun `socket proof bytes bind generation nonce and connection`() {
        val first = AndroidAutomationIdentityManager.socketSigningBytes(
            connectionId = "00000000-0000-0000-0000-000000000001",
            keyId = "test-key-id-123456",
            targetRef = "android-device:opaque",
            reviewGeneration = 7,
            protocolVersion = "2026-07-28",
            serverNonce = ByteArray(32) { 3 },
            apkSha256 = "b".repeat(64),
            attestationPolicyDigest = "blake3:" + "c".repeat(64),
        )
        assertFalse(first.isEmpty())
        assertNotEquals(
            first.toList(),
            AndroidAutomationIdentityManager.socketSigningBytes(
                connectionId = "00000000-0000-0000-0000-000000000002",
                keyId = "test-key-id-123456",
                targetRef = "android-device:opaque",
                reviewGeneration = 7,
                protocolVersion = "2026-07-28",
                serverNonce = ByteArray(32) { 3 },
                apkSha256 = "b".repeat(64),
                attestationPolicyDigest = "blake3:" + "c".repeat(64),
            ).toList(),
        )
        assertNotEquals(
            first.toList(),
            AndroidAutomationIdentityManager.socketSigningBytes(
                connectionId = "00000000-0000-0000-0000-000000000001",
                keyId = "test-key-id-123456",
                targetRef = "android-device:opaque",
                reviewGeneration = 8,
                protocolVersion = "2026-07-28",
                serverNonce = ByteArray(32) { 3 },
                apkSha256 = "b".repeat(64),
                attestationPolicyDigest = "blake3:" + "c".repeat(64),
            ).toList(),
        )
    }
}
