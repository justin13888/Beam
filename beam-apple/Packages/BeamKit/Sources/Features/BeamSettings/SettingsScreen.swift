import BeamCore
import BeamDesignSystem
import BeamFFI
import BeamModel
import BeamUI
import SwiftUI

/// Preferences, account and trust.
public struct SettingsScreen: View {
    @State private var model: SettingsModel
    private let onSignedOut: () -> Void
    private let onOpenAdmin: () -> Void
    private let onOpenHistory: () -> Void

    /// Build the screen over a model.
    public init(
        model: SettingsModel,
        onSignedOut: @escaping () -> Void,
        onOpenAdmin: @escaping () -> Void,
        onOpenHistory: @escaping () -> Void
    ) {
        _model = State(wrappedValue: model)
        self.onSignedOut = onSignedOut
        self.onOpenAdmin = onOpenAdmin
        self.onOpenHistory = onOpenHistory
    }

    public var body: some View {
        Form {
            appearance
            playback
            downloads
            account
            security
        }
        .formStyle(.grouped)
        .navigationTitle("Settings")
        .task { await model.load() }
        .alert(
            "Something went wrong",
            isPresented: .constant(model.actionMessage != nil),
            presenting: model.actionMessage
        ) { _ in
            Button("OK") { model.actionMessage = nil }
        } message: { message in
            Text(message)
        }
    }

    private var appearance: some View {
        Section("Appearance") {
            Picker("Theme", selection: $model.preferences.theme) {
                Text("System").tag(ThemeMode.system)
                Text("Light").tag(ThemeMode.light)
                Text("Dark").tag(ThemeMode.dark)
            }
        }
    }

    private var playback: some View {
        Section {
            Picker("Quality", selection: $model.preferences.quality) {
                Text("Best available").tag(QualityPreference.best)
                Text("Match this screen").tag(QualityPreference.matchScreen)
                Text("Smallest file").tag(QualityPreference.smallest)
            }
            Toggle("Autoplay next episode", isOn: $model.preferences.autoplayNextEpisode)
            Toggle("Allow software decoding", isOn: $model.preferences.allowSoftwareDecode)
        } header: {
            Text("Playback")
        } footer: {
            Text(
                """
                Beam never re-encodes: it plays the file as it is on the server. \
                Software decoding lets more files play, at the cost of battery \
                and heat.
                """
            )
        }
    }

    private var downloads: some View {
        Section("Downloads") {
            Toggle("Download over cellular", isOn: $model.preferences.allowCellularDownloads)
        }
    }

    private var account: some View {
        Section("Account") {
            if let server = model.activeServer {
                LabeledContent("Server", value: server.displayName)
                if case .authenticated(let user) = server.state {
                    LabeledContent("Signed in as", value: user.displayName)
                    if user.isAdmin {
                        Button("Admin", action: onOpenAdmin)
                    }
                }
            }
            Button("Watch history", action: onOpenHistory)

            sessionList

            Button("Sign out", role: .destructive) {
                Task {
                    await model.signOut()
                    onSignedOut()
                }
            }
            Button("Sign out everywhere", role: .destructive) {
                Task {
                    await model.signOutEverywhere()
                    onSignedOut()
                }
            }
        }
    }

    @ViewBuilder
    private var sessionList: some View {
        switch model.sessions {
        case .idle, .loading:
            ProgressView()
        case .failed(let message):
            Text(message).font(.caption).foregroundStyle(BeamTheme.Colors.caution)
        case .loaded(let sessions):
            ForEach(sessions, id: \.id) { session in
                HStack {
                    VStack(alignment: .leading) {
                        Text(session.ip).font(.callout)
                        Text(
                            "Last active \(Date(timeIntervalSince1970: TimeInterval(session.lastActiveUnix)).formatted(.relative(presentation: .named)))"
                        )
                        .font(.caption)
                        .foregroundStyle(BeamTheme.Colors.onGlassSecondary)
                    }
                    Spacer()
                    Button("Revoke", role: .destructive) {
                        Task { await model.revoke(sessionId: session.id) }
                    }
                    .buttonStyle(.borderless)
                }
            }
        }
    }

    private var security: some View {
        Section {
            if model.trustedFingerprints.isEmpty {
                Text("No certificates have been accepted for this server.")
                    .font(.caption)
                    .foregroundStyle(BeamTheme.Colors.onGlassSecondary)
            } else {
                ForEach(model.trustedFingerprints, id: \.self) { fingerprint in
                    Text(fingerprint).font(.caption.monospaced()).textSelection(.enabled)
                }
                Button("Forget accepted certificates", role: .destructive) {
                    Task { await model.forgetCertificates() }
                }
            }
        } header: {
            Text("Security")
        } footer: {
            Text(
                """
                Accepting a certificate widens trust for that one certificate on \
                that one host. It can never override your device's own trust \
                decisions.
                """
            )
        }
    }
}
