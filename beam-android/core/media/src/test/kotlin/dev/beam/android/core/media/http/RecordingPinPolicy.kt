package dev.beam.android.core.media.http

/**
 * Stands in for the core's pin rule, which lives in a native library the JVM
 * tests do not load. The rule itself is tested in the core
 * (`tls::tests::a_pin_is_honoured_only_for_its_host_and_window`); what these
 * tests hold to account is when the trust manager asks it, and with what.
 */
internal class RecordingPinPolicy(
    private val accepts: Boolean,
) : PinnedCertificatePolicy {
    class Asked(
        val der: ByteArray,
        val host: String,
        val trustedFingerprints: List<String>,
        val nowUnix: Long,
    )

    private val recorded = java.util.concurrent.CopyOnWriteArrayList<Asked>()

    val asked: List<Asked> get() = recorded.toList()

    override fun accepts(
        der: ByteArray,
        host: String,
        trustedFingerprints: List<String>,
        nowUnix: Long,
    ): Boolean {
        recorded += Asked(der, host, trustedFingerprints, nowUnix)
        return accepts
    }
}
