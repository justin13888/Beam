import BeamCore
import BeamDesignSystem
import BeamFFI
import SwiftUI

/// Ask the user to accept one certificate, for one host.
///
/// The whole-certificate SHA-256 is shown in the colon-grouped uppercase hex
/// `openssl x509 -fingerprint -sha256` prints, so the string here is the same
/// string on the server -- a trust decision the user cannot independently
/// verify is theatre.
///
/// Accepting widens trust for exactly this certificate on exactly this host.
/// It can never reject a publicly valid certificate and never generalises: the
/// system trust store is always consulted first and its acceptance is final.
public struct TrustPrompt: View {
    private let host: String
    private let details: CertificateDetails
    private let onAccept: () -> Void
    private let onReject: () -> Void

    /// Build the prompt.
    public init(
        host: String,
        details: CertificateDetails,
        onAccept: @escaping () -> Void,
        onReject: @escaping () -> Void
    ) {
        self.host = host
        self.details = details
        self.onAccept = onAccept
        self.onReject = onReject
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: BeamTheme.Spacing.regular) {
            Label("Unrecognised certificate", systemImage: "lock.trianglebadge.exclamationmark")
                .font(BeamTheme.Typography.sectionTitle)

            Text(
                """
                \(host) presented a certificate your device does not trust. \
                This is normal for a server on your own network with a \
                self-signed certificate, and is not normal for one on the \
                public internet.
                """
            )
            .font(.footnote)

            VStack(alignment: .leading, spacing: BeamTheme.Spacing.small) {
                field("Subject", details.subject)
                field("Issuer", details.issuer)
                field("SHA-256", details.sha256Fingerprint, monospaced: true)
                if details.isExpired {
                    Label("This certificate has expired", systemImage: "exclamationmark.triangle")
                        .font(.caption)
                        .foregroundStyle(BeamTheme.Colors.caution)
                }
            }
            .beamGlassPadding()
            .beamGlassPanel()

            Text("Compare this fingerprint against your server before accepting.")
                .font(.caption)
                .foregroundStyle(BeamTheme.Colors.onGlassSecondary)

            HStack {
                Button("Don't connect", role: .cancel, action: onReject)
                    .buttonStyle(.glass)
                Spacer()
                Button("Trust this certificate", action: onAccept)
                    .buttonStyle(.glassProminent)
                    // An expired certificate is refused outright rather than
                    // offered with a warning: the core will reject it anyway,
                    // and offering the button would be a promise the client
                    // cannot keep.
                    .disabled(details.isExpired)
            }
        }
        .padding(BeamTheme.Spacing.loose)
        .frame(maxWidth: 520)
    }

    private func field(_ label: String, _ value: String, monospaced: Bool = false) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label)
                .font(.caption2)
                .foregroundStyle(BeamTheme.Colors.onGlassSecondary)
            Text(value)
                .font(monospaced ? .caption.monospaced() : .caption)
                .textSelection(.enabled)
        }
    }
}
