import BeamCore
import BeamDesignSystem
import BeamFFI
import BeamModel
import BeamUI
import SwiftUI

/// Every file behind a title, with what this device can do with each.
///
/// Unplayable sources are shown, not hidden. Under direct play that is a
/// permanent property of the file rather than a transient failure, and a
/// viewer who can see "this rip is VC-1, which this device cannot decode" can
/// act on it -- by picking another rip, or by adding one. A picker that
/// silently omitted it would look like the file was missing.
public struct SourcePicker: View {
    private let sources: [MediaSourceView]
    private let selection: SourceSelection?
    private let chosenFileId: String?
    private let onChoose: (String) -> Void

    /// Build the picker.
    public init(
        sources: [MediaSourceView],
        selection: SourceSelection?,
        chosenFileId: String?,
        onChoose: @escaping (String) -> Void
    ) {
        self.sources = sources
        self.selection = selection
        self.chosenFileId = chosenFileId
        self.onChoose = onChoose
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: BeamTheme.Spacing.compact) {
            Text("Versions")
                .font(BeamTheme.Typography.sectionTitle)

            ForEach(sources, id: \.fileId) { source in
                let rejection = rejection(for: source)
                Button {
                    onChoose(source.fileId)
                } label: {
                    SourceRow(
                        source: source,
                        rejection: rejection,
                        playability: playability(for: source),
                        isChosen: isChosen(source)
                    )
                }
                .buttonStyle(.plain)
                .disabled(rejection != nil)
            }
        }
    }

    private func rejection(for source: MediaSourceView) -> RejectedSource? {
        selection?.rejected.first { $0.fileId == source.fileId }
    }

    private func playability(for source: MediaSourceView) -> Playability? {
        selection?.source.fileId == source.fileId ? selection?.playability : nil
    }

    private func isChosen(_ source: MediaSourceView) -> Bool {
        (chosenFileId ?? selection?.source.fileId) == source.fileId
    }
}

/// One file, with its badges.
struct SourceRow: View {
    let source: MediaSourceView
    let rejection: RejectedSource?
    let playability: Playability?
    let isChosen: Bool

    var body: some View {
        HStack(alignment: .top, spacing: BeamTheme.Spacing.compact) {
            Image(systemName: isChosen ? "checkmark.circle.fill" : "circle")
                .foregroundStyle(
                    rejection == nil ? BeamTheme.Colors.accent : BeamTheme.Colors.unavailable
                )

            VStack(alignment: .leading, spacing: BeamTheme.Spacing.small) {
                HStack(spacing: BeamTheme.Spacing.small) {
                    if let resolution = BeamFormat.resolution(
                        width: source.width,
                        height: source.height
                    ) {
                        BeamBadge(resolution)
                    }
                    if let codec = source.videoCodec {
                        BeamBadge(codec.uppercased())
                    }
                    if let container = source.container {
                        BeamBadge(container.uppercased())
                    }
                    if let hdr = source.hdrFormat {
                        BeamBadge(hdr, systemImage: "sun.max", emphasis: .positive)
                    }
                }

                HStack(spacing: BeamTheme.Spacing.small) {
                    Text(BeamFormat.fileSize(bytes: source.sizeBytes))
                    if let bitrate = BeamFormat.bitrate(bitsPerSecond: source.bitRate) {
                        Text(bitrate)
                    }
                    if !source.audioTracks.isEmpty {
                        Text(audioSummary)
                    }
                }
                .font(.caption)
                .foregroundStyle(BeamTheme.Colors.onGlassSecondary)

                statusBadge
            }
            Spacer()
        }
        .padding(BeamTheme.Spacing.compact)
        .beamGlassPanel(
            shape: RoundedRectangle(cornerRadius: BeamTheme.Radius.medium, style: .continuous),
            interactive: rejection == nil
        )
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder
    private var statusBadge: some View {
        if let rejection {
            BeamBadge(
                rejection.detail,
                systemImage: "exclamationmark.triangle",
                emphasis: .unavailable
            )
        } else {
            switch playability {
            case .hardware:
                BeamBadge("Hardware decode", systemImage: "bolt", emphasis: .positive)
            case .software(let detail):
                // Not a warning for its own sake: a software decode of a 4K
                // HEVC stream is a hot phone and a flat battery, and the
                // viewer is the one who should decide whether that is worth it.
                BeamBadge(detail, systemImage: "cpu", emphasis: .caution)
            case .unsupported(let reason, let detail):
                BeamBadge(
                    detail.isEmpty ? String(describing: reason) : detail,
                    systemImage: "exclamationmark.triangle",
                    emphasis: .unavailable
                )
            case nil:
                EmptyView()
            }
        }
    }

    private var audioSummary: String {
        let codecs = Set(source.audioTracks.map { $0.codec.uppercased() }).sorted()
        return codecs.joined(separator: ", ")
    }
}
