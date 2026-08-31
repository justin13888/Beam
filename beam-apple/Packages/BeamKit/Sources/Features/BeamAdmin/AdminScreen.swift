import BeamCore
import BeamDesignSystem
import BeamFFI
import BeamModel
import BeamUI
import SwiftUI

/// The operator screen.
public struct AdminScreen: View {
    @State private var model: AdminModel

    /// Build the screen over a model.
    public init(model: AdminModel) {
        _model = State(wrappedValue: model)
    }

    public var body: some View {
        content
            .navigationTitle("Admin")
            .task { await model.load() }
            .refreshable { await model.load() }
            .alert(
                "Server said",
                isPresented: .constant(model.actionMessage != nil),
                presenting: model.actionMessage
            ) { _ in
                Button("OK") { model.actionMessage = nil }
            } message: { message in
                Text(message)
            }
    }

    @ViewBuilder
    private var content: some View {
        if model.isForbidden {
            BeamStateView(
                .empty(
                    title: "Not an administrator",
                    message: "This account cannot manage the server.",
                    systemImage: "lock"
                )
            )
        } else {
            switch model.status {
            case .idle, .loading:
                BeamStateView(.loading)
            case .failed(let message):
                BeamStateView(.failed(message: message, isRetryable: true)) {
                    Task { await model.load() }
                }
            case .loaded(let status):
                Form {
                    statusSection(status)
                    librariesSection
                    usersSection
                    logSection
                }
                .formStyle(.grouped)
            }
        }
    }

    private func statusSection(_ status: AdminStatus) -> some View {
        Section("Server") {
            LabeledContent("Version", value: status.version)
            LabeledContent(
                "Uptime",
                value: BeamFormat.duration(seconds: Double(status.uptimeSecs))
            )
            LabeledContent("Libraries", value: "\(status.counts.libraries)")
            LabeledContent("Files", value: "\(status.counts.files)")
            LabeledContent("Users", value: "\(status.counts.users)")
            LabeledContent(
                "Enrichment",
                value: "\(status.enrichment.enriched) done, \(status.enrichment.pending) pending"
            )
            if status.enrichment.unmatched > 0 {
                Label(
                    "\(status.enrichment.unmatched) titles could not be matched",
                    systemImage: "questionmark.circle"
                )
                .font(.caption)
                .foregroundStyle(BeamTheme.Colors.caution)
            }
        }
    }

    private var librariesSection: some View {
        Section("Libraries") {
            ForEach(model.libraries, id: \.id) { library in
                HStack {
                    VStack(alignment: .leading) {
                        Text(library.name).font(.headline)
                        Text("\(library.size) titles")
                            .font(.caption)
                            .foregroundStyle(BeamTheme.Colors.onGlassSecondary)
                    }
                    Spacer()
                    Button("Scan") { Task { await model.scan(libraryId: library.id) } }
                        .buttonStyle(.borderless)
                }
                .swipeActions {
                    Button("Delete", role: .destructive) {
                        Task { await model.deleteLibrary(id: library.id) }
                    }
                }
            }

            LabeledContent("Name") {
                TextField("Films", text: $model.newLibraryName)
                    .multilineTextAlignment(.trailing)
            }
            LabeledContent("Path on server") {
                TextField("/media/films", text: $model.newLibraryPath)
                    .multilineTextAlignment(.trailing)
                    .autocorrectionDisabled()
            }
            Button("Add library") { Task { await model.createLibrary() } }
                .disabled(model.newLibraryName.isEmpty || model.newLibraryPath.isEmpty)
        }
    }

    private var usersSection: some View {
        Section("Users") {
            ForEach(model.users, id: \.id) { user in
                HStack {
                    VStack(alignment: .leading) {
                        Text(user.displayName).font(.callout)
                        if let email = user.email {
                            Text(email)
                                .font(.caption)
                                .foregroundStyle(BeamTheme.Colors.onGlassSecondary)
                        }
                    }
                    Spacer()
                    if user.isAdmin {
                        BeamBadge("Admin", emphasis: .positive)
                    }
                    Toggle(
                        "Enabled",
                        isOn: Binding(
                            get: { !user.disabled },
                            set: { enabled in
                                Task { await model.setDisabled(!enabled, userId: user.id) }
                            }
                        )
                    )
                    .labelsHidden()
                }
            }
        }
    }

    private var logSection: some View {
        Section("Recent log") {
            ForEach(model.logs, id: \.id) { entry in
                VStack(alignment: .leading, spacing: BeamTheme.Spacing.tight) {
                    HStack {
                        BeamBadge(
                            String(describing: entry.level).uppercased(),
                            emphasis: emphasis(for: entry.level)
                        )
                        Text(entry.category)
                            .font(.caption)
                            .foregroundStyle(BeamTheme.Colors.onGlassSecondary)
                    }
                    Text(entry.message).font(.caption)
                }
            }
        }
    }

    private func emphasis(for level: LogLevel) -> BeamBadge.Emphasis {
        switch level {
        case .error: .unavailable
        case .warning: .caution
        default: .neutral
        }
    }
}
