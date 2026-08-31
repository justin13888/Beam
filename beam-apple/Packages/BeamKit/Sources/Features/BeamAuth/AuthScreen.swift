import BeamCore
import BeamDesignSystem
import BeamFFI
import BeamModel
import BeamUI
import SwiftUI

/// Connect to a server and sign in.
public struct AuthScreen: View {
    @State private var model: AuthModel
    private let onSignedIn: () -> Void

    /// Build the screen over a model.
    public init(model: AuthModel, onSignedIn: @escaping () -> Void) {
        _model = State(wrappedValue: model)
        self.onSignedIn = onSignedIn
    }

    public var body: some View {
        Group {
            if case .signingIn(let url, let serverId) = model.phase {
                SignInWebView(url: url, host: url.host() ?? "") { cookie in
                    Task { await model.completeSignIn(serverId: serverId, cookie: cookie) }
                }
                .ignoresSafeArea()
                .overlay(alignment: .topTrailing) {
                    Button("Cancel") { model.cancelSignIn() }
                        .buttonStyle(.glass)
                        .padding()
                }
            } else {
                connectForm
            }
        }
        .task { await model.load() }
        .onChange(of: model.phase) { _, phase in
            if case .signedIn = phase { onSignedIn() }
        }
        .sheet(isPresented: .constant(model.pendingTrust != nil)) {
            if let pending = model.pendingTrust {
                TrustPrompt(
                    host: pending.host,
                    details: pending.details,
                    onAccept: { Task { await model.acceptPendingCertificate() } },
                    onReject: { model.rejectPendingCertificate() }
                )
            }
        }
    }

    private var connectForm: some View {
        VStack(spacing: BeamTheme.Spacing.loose) {
            Spacer()

            Image(systemName: "play.tv")
                .font(.system(size: 64))
                .foregroundStyle(BeamTheme.Colors.accent)

            Text("Connect to Beam")
                .font(BeamTheme.Typography.screenTitle)

            VStack(spacing: BeamTheme.Spacing.compact) {
                TextField("https://beam.example.com", text: $model.address)
                    .textFieldStyle(.plain)
                    .textContentType(.URL)
                    #if os(iOS)
                .keyboardType(.URL)
                .textInputAutocapitalization(.never)
                    #endif
                    .autocorrectionDisabled()
                    .beamGlassPadding()
                    .beamGlassPanel(shape: Capsule())

                Button {
                    Task { await model.connect() }
                } label: {
                    if case .connecting = model.phase {
                        ProgressView().frame(maxWidth: .infinity)
                    } else {
                        Text("Connect").frame(maxWidth: .infinity)
                    }
                }
                .buttonStyle(.glassProminent)
                .disabled(model.address.trimmingCharacters(in: .whitespaces).isEmpty)
            }
            .frame(maxWidth: 420)

            if case .failed(let message) = model.phase {
                Label(message, systemImage: "exclamationmark.triangle")
                    .font(.footnote)
                    .foregroundStyle(BeamTheme.Colors.caution)
                    .multilineTextAlignment(.center)
            }

            if !model.servers.isEmpty {
                knownServers
            }

            Spacer()
        }
        .padding(BeamTheme.Spacing.loose)
    }

    private var knownServers: some View {
        VStack(alignment: .leading, spacing: BeamTheme.Spacing.small) {
            Text("Known servers")
                .font(BeamTheme.Typography.sectionTitle)
            ForEach(model.servers, id: \.id) { server in
                Button {
                    Task { await model.signIn(to: server) }
                } label: {
                    HStack {
                        VStack(alignment: .leading) {
                            Text(server.displayName).font(.headline)
                            Text(server.baseUrl)
                                .font(.caption)
                                .foregroundStyle(BeamTheme.Colors.onGlassSecondary)
                        }
                        Spacer()
                        Image(systemName: "chevron.right").font(.footnote)
                    }
                    .beamGlassPadding()
                    .beamGlassPanel(interactive: true)
                }
                .buttonStyle(.plain)
            }
        }
        .frame(maxWidth: 420)
    }
}
