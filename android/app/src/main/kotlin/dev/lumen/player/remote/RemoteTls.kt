package dev.lumen.player.remote

import java.security.MessageDigest
import java.security.cert.CertificateException
import java.security.cert.X509Certificate
import javax.net.ssl.X509TrustManager

/**
 * Trust-on-first-use, matched to `lumen serve`'s own model (see the Rust side's `remote/tls.rs`).
 *
 * There is no certificate authority to check against on a home LAN, and no hostname either -- the
 * server's entire identity, as far as this app is concerned, is the SHA-256 fingerprint of the
 * certificate it presents. Two modes, selected by whether [pinned] is null:
 *
 * - **First pairing** ([pinned] is `null`): any certificate is accepted, and its fingerprint is
 *   recorded in [observedFingerprint] for the caller to persist -- but only *after* the pairing code
 *   exchange itself succeeds. The code is a second, independent secret shown only on the server's own
 *   terminal, so an attacker would need to intercept that too, not just this connection, to plant a
 *   certificate that then gets trusted.
 * - **Reconnecting** ([pinned] is a saved fingerprint): the presented certificate must match exactly,
 *   or the connection is refused. This is the check that actually catches a swapped-in server on a
 *   later connection -- first-pairing trust alone would be worthless if every reconnect re-trusted
 *   on sight instead of holding the server to what it showed the first time.
 */
class FingerprintTrustManager(private val pinned: String?) : X509TrustManager {

    var observedFingerprint: String? = null
        private set

    override fun checkClientTrusted(chain: Array<out X509Certificate>, authType: String) {
        // This app is never the one presenting a certificate; a client cert here would mean the
        // handshake is being driven in a shape this trust manager was never meant to arbitrate.
        throw CertificateException("client certificates are not expected or supported")
    }

    override fun checkServerTrusted(chain: Array<out X509Certificate>, authType: String) {
        val cert = chain.firstOrNull()
            ?: throw CertificateException("the server did not present a certificate")
        val digest = MessageDigest.getInstance("SHA-256").digest(cert.encoded)
        val hex = digest.joinToString("") { "%02x".format(it) }
        observedFingerprint = hex

        val expected = pinned
        if (expected != null && !hex.equals(expected, ignoreCase = true)) {
            throw CertificateException(
                "server certificate fingerprint changed: expected $expected but got $hex -- " +
                    "refusing to connect. This can mean the server was reinstalled (re-pair to trust " +
                    "the new one), or that something else on this network is impersonating it.",
            )
        }
    }

    override fun getAcceptedIssuers(): Array<X509Certificate> = arrayOf()
}
