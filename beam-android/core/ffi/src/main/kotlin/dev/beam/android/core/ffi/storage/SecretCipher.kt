package dev.beam.android.core.ffi.storage

import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Encrypts session cookies with a key held in the Android keystore.
 *
 * The key material never leaves the keystore, so a rooted-device dump or a
 * backup extraction yields ciphertext rather than a usable session. This is
 * hand-rolled rather than using `androidx.security:security-crypto` because
 * that library is deprecated and unmaintained; AES-GCM through the platform
 * keystore is the mechanism it wrapped anyway.
 */
internal object SecretCipher {
    private const val PROVIDER = "AndroidKeyStore"
    private const val ALIAS = "dev.beam.android.session"
    private const val TRANSFORMATION = "AES/GCM/NoPadding"
    private const val TAG_LENGTH_BITS = 128
    private const val IV_LENGTH_BYTES = 12

    /** Encrypt to `base64(iv || ciphertext)`. */
    fun encrypt(plaintext: String): String {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, key())
        val encrypted = cipher.doFinal(plaintext.toByteArray(Charsets.UTF_8))
        val packed = cipher.iv + encrypted
        return Base64.encodeToString(packed, Base64.NO_WRAP)
    }

    /**
     * Decrypt a value produced by [encrypt].
     *
     * Returns null rather than throwing when the value cannot be read. That
     * happens for real and recoverable reasons -- the user added a lock screen
     * and invalidated the key, or the app was restored onto another device --
     * and the right response is to treat the session as expired and ask them
     * to sign in, not to crash on launch.
     */
    fun decrypt(encoded: String): String? =
        runCatching {
            val packed = Base64.decode(encoded, Base64.NO_WRAP)
            require(packed.size > IV_LENGTH_BYTES) { "ciphertext is too short to contain an IV" }
            val iv = packed.copyOfRange(0, IV_LENGTH_BYTES)
            val body = packed.copyOfRange(IV_LENGTH_BYTES, packed.size)
            val cipher = Cipher.getInstance(TRANSFORMATION)
            cipher.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(TAG_LENGTH_BITS, iv))
            cipher.doFinal(body).toString(Charsets.UTF_8)
        }.getOrNull()

    private fun key(): SecretKey {
        val store = KeyStore.getInstance(PROVIDER).apply { load(null) }
        (store.getEntry(ALIAS, null) as? KeyStore.SecretKeyEntry)?.let { return it.secretKey }

        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, PROVIDER)
        generator.init(
            KeyGenParameterSpec
                .Builder(
                    ALIAS,
                    KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
                ).setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                // Deliberately not requiring user authentication: playback has
                // to survive a locked screen, and the threat this defends
                // against is offline extraction of the stored file.
                .setUserAuthenticationRequired(false)
                .build(),
        )
        return generator.generateKey()
    }
}
