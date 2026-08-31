package dev.beam.android.feature.auth

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import dev.beam.android.core.designsystem.BeamSpacing
import java.text.DateFormat
import java.util.Date

/**
 * Asks the viewer whether to trust a certificate the platform rejected.
 *
 * Deliberately not a yes/no with a reassuring summary. A trust decision the
 * viewer cannot actually verify is theatre, so this shows the fingerprint in
 * the same form `openssl x509 -fingerprint -sha256` prints, in a monospace
 * face, so it can be compared character by character against what their server
 * reports. Everything else is context for that comparison.
 *
 * The dismissive action is the default and the accepting one is styled as the
 * lesser choice, because the common case for an unexpected certificate is a
 * mistake, not an attack -- but the rare case is an attack.
 */
@Composable
internal fun TrustPrompt(
    trust: PendingTrust,
    onAccept: () -> Unit,
    onDecline: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val details = trust.details
    AlertDialog(
        modifier = modifier,
        onDismissRequest = onDecline,
        title = { Text("Check this server's certificate") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(BeamSpacing.Compact)) {
                Text(
                    text =
                        if (details.isSelfSigned) {
                            "${trust.host} identified itself with a certificate it signed " +
                                "itself. That is normal for a server on your own network, and " +
                                "also what an impostor would do."
                        } else {
                            "${trust.host} presented a certificate your device does not " +
                                "recognise."
                        },
                    style = MaterialTheme.typography.bodyMedium,
                )

                if (details.isExpired) {
                    Text(
                        text = "This certificate has expired.",
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.error,
                    )
                }

                Text(
                    text = "Compare this with the fingerprint your server reports:",
                    style = MaterialTheme.typography.labelLarge,
                    modifier = Modifier.padding(top = BeamSpacing.Small),
                )
                Text(
                    text = details.sha256Fingerprint,
                    style =
                        MaterialTheme.typography.bodySmall.copy(
                            fontFamily = FontFamily.Monospace,
                        ),
                    modifier = Modifier.fillMaxWidth(),
                )

                CertificateField("Issued to", details.subject)
                CertificateField("Issued by", details.issuer)
                CertificateField(
                    label = "Valid until",
                    value =
                        DateFormat
                            .getDateInstance()
                            .format(Date(details.notAfterUnix * MILLIS_PER_SECOND)),
                )
                if (details.subjectAltNames.isNotEmpty()) {
                    CertificateField(
                        label = "Valid for",
                        value = details.subjectAltNames.joinToString(", "),
                    )
                }
            }
        },
        confirmButton = {
            TextButton(onClick = onDecline) { Text("Don't connect") }
        },
        dismissButton = {
            TextButton(onClick = onAccept) { Text("Trust this certificate") }
        },
    )
}

@Composable
private fun CertificateField(
    label: String,
    value: String,
) {
    Column {
        Text(
            text = label,
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text(text = value, style = MaterialTheme.typography.bodySmall)
    }
}

private const val MILLIS_PER_SECOND = 1_000L
