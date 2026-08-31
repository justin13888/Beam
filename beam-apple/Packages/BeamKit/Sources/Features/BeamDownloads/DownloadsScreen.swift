import BeamCore
import BeamDesignSystem
import BeamModel
import BeamPlayback
import BeamUI
import SwiftUI

/// Offline downloads, grouped by what can be done with them.
public struct DownloadsScreen: View {
    private let coordinator: DownloadCoordinator
    private let onPlay: (DownloadRecord) -> Void

    /// Build the screen over the coordinator.
    public init(coordinator: DownloadCoordinator, onPlay: @escaping (DownloadRecord) -> Void) {
        self.coordinator = coordinator
        self.onPlay = onPlay
    }

    public var body: some View {
        content.navigationTitle("Downloads")
    }

    @ViewBuilder
    private var content: some View {
        if coordinator.records.isEmpty {
            BeamStateView(
                .empty(
                    title: "No downloads",
                    message: "Download a title from its page to watch it offline.",
                    systemImage: "arrow.down.circle"
                )
            )
        } else {
            List {
                // Grouped by state rather than shown flat: the three groups
                // afford different actions, and a failed download buried
                // between two completed ones is one nobody retries.
                section("Ready to watch", records: completed)
                section("Downloading", records: inProgress)
                section("Failed", records: failed)
            }
            .listStyle(.plain)
            .beamScrollEdges()
        }
    }

    @ViewBuilder
    private func section(_ title: String, records: [DownloadRecord]) -> some View {
        if !records.isEmpty {
            Section(title) {
                ForEach(records) { record in
                    DownloadRow(
                        record: record,
                        onPlay: { onPlay(record) },
                        onPause: { coordinator.pause(fileId: record.fileId) },
                        onResume: { coordinator.resume(fileId: record.fileId) },
                        onRemove: { coordinator.remove(fileId: record.fileId) }
                    )
                }
            }
        }
    }

    private var completed: [DownloadRecord] {
        coordinator.records.filter { $0.state == .completed }
    }

    private var inProgress: [DownloadRecord] {
        coordinator.records.filter { record in
            switch record.state {
            case .queued, .downloading, .paused: true
            default: false
            }
        }
    }

    private var failed: [DownloadRecord] {
        coordinator.records.filter { record in
            if case .failed = record.state { return true }
            return false
        }
    }
}

/// One download.
struct DownloadRow: View {
    let record: DownloadRecord
    let onPlay: () -> Void
    let onPause: () -> Void
    let onResume: () -> Void
    let onRemove: () -> Void

    var body: some View {
        HStack(spacing: BeamTheme.Spacing.compact) {
            VStack(alignment: .leading, spacing: BeamTheme.Spacing.tight) {
                Text(record.title).font(.headline).lineLimit(1)
                if let subtitle = record.subtitle {
                    Text(subtitle)
                        .font(.caption)
                        .foregroundStyle(BeamTheme.Colors.onGlassSecondary)
                }
                status
            }
            Spacer()
            action
        }
        .padding(.vertical, BeamTheme.Spacing.tight)
        .swipeActions {
            Button("Remove", role: .destructive, action: onRemove)
        }
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder
    private var status: some View {
        switch record.state {
        case .completed:
            HStack(spacing: BeamTheme.Spacing.small) {
                BeamBadge("Offline", systemImage: "checkmark.circle", emphasis: .positive)
                if let total = record.totalBytes {
                    Text(BeamFormat.fileSize(bytes: total))
                        .font(.caption)
                        .foregroundStyle(BeamTheme.Colors.onGlassSecondary)
                }
            }
        case .queued:
            Text("Waiting").font(.caption)
                .foregroundStyle(BeamTheme.Colors.onGlassSecondary)
        case .downloading(let fraction), .paused(let fraction):
            ProgressView(value: fraction ?? record.fraction)
                .progressViewStyle(.linear)
                .tint(BeamTheme.Colors.accent)
        case .failed(let message):
            Label(message, systemImage: "exclamationmark.triangle")
                .font(.caption)
                .foregroundStyle(BeamTheme.Colors.caution)
                .lineLimit(2)
        }
    }

    @ViewBuilder
    private var action: some View {
        switch record.state {
        case .completed:
            Button("Play", action: onPlay).buttonStyle(.glass)
        case .downloading:
            Button {
                onPause()
            } label: {
                Image(systemName: "pause.fill")
            }
            .buttonStyle(.glass)
        case .paused, .failed:
            Button {
                onResume()
            } label: {
                Image(systemName: "arrow.clockwise")
            }
            .buttonStyle(.glass)
        case .queued:
            ProgressView()
        }
    }
}
