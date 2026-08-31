import BeamCore
import BeamDesignSystem
import BeamFFI
import BeamModel
import BeamPlayback
import BeamUI
import SwiftUI

/// The fullscreen player.
///
/// The controls are Beam's rather than the system's even when `AVPlayerEngine`
/// is driving, so the transport looks and behaves the same whichever engine is
/// underneath -- a viewer should not be able to tell which container they
/// picked. The system chrome is still there underneath for Picture in Picture
/// and AirPlay, which `SampleBufferEngine` cannot offer.
public struct PlayerScreen: View {
    @State private var model: PlayerModel
    @State private var surface: AnyViewBox?
    private let item: PlaybackItem
    private let onClose: () -> Void
    private let onPlayNext: (EpisodeSummary) -> Void

    /// Build the screen.
    public init(
        model: PlayerModel,
        item: PlaybackItem,
        onClose: @escaping () -> Void,
        onPlayNext: @escaping (EpisodeSummary) -> Void
    ) {
        _model = State(wrappedValue: model)
        self.item = item
        self.onClose = onClose
        self.onPlayNext = onPlayNext
    }

    public var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()

            if let surface {
                surface.view
            }

            if let text = model.snapshot.activeSubtitleText {
                subtitleOverlay(text)
            }

            if case .failed(let message) = model.snapshot.status {
                failure(message)
            } else if model.areControlsVisible {
                controls
            }

            if model.shouldOfferUpNext, let next = model.upNext {
                upNextCard(next)
            }
        }
        .task {
            surface = model.videoView()
            await model.start(item: item)
        }
        .onDisappear {
            Task { await model.stop() }
        }
        #if os(iOS)
        .statusBarHidden(!model.areControlsVisible)
        .persistentSystemOverlays(model.areControlsVisible ? .automatic : .hidden)
        #endif
        .onTapGesture {
            withAnimation(.smooth) { model.areControlsVisible.toggle() }
        }
    }

    private var controls: some View {
        VStack {
            HStack {
                Button {
                    onClose()
                } label: {
                    Label("Close", systemImage: "xmark")
                        .labelStyle(.iconOnly)
                }
                .buttonStyle(.glass)

                VStack(alignment: .leading, spacing: 0) {
                    Text(model.request.title).font(.headline).lineLimit(1)
                    if let subtitle = model.request.subtitle {
                        Text(subtitle)
                            .font(.caption)
                            .foregroundStyle(BeamTheme.Colors.onGlassSecondary)
                            .lineLimit(1)
                    }
                }
                Spacer()
                trackMenu
            }
            .padding(BeamTheme.Spacing.regular)

            Spacer()

            BeamGlassGroup {
                VStack(spacing: BeamTheme.Spacing.compact) {
                    scrubber
                    transport
                }
                .beamGlassPadding()
            }
            .padding(BeamTheme.Spacing.regular)
        }
        .transition(.opacity)
    }

    private var scrubber: some View {
        VStack(spacing: BeamTheme.Spacing.tight) {
            Slider(
                value: Binding(
                    get: { model.snapshot.position },
                    set: { model.seek(to: $0) }
                ),
                in: 0...max(model.snapshot.duration ?? 1, 1)
            )
            .disabled(!model.snapshot.isSeekable)

            HStack {
                Text(BeamFormat.timecode(seconds: model.snapshot.position))
                Spacer()
                if let duration = model.snapshot.duration {
                    Text(
                        "-\(BeamFormat.timecode(seconds: max(0, duration - model.snapshot.position)))"
                    )
                }
            }
            .font(.caption.monospacedDigit())
            .foregroundStyle(BeamTheme.Colors.onGlassSecondary)
        }
    }

    private var transport: some View {
        HStack(spacing: BeamTheme.Spacing.loose) {
            Button {
                model.skip(by: -NowPlayingCenter.skipInterval)
            } label: {
                Image(systemName: "gobackward.15")
            }
            .buttonStyle(.glass)

            Button {
                model.togglePlayPause()
            } label: {
                Image(systemName: isPlaying ? "pause.fill" : "play.fill")
                    .font(.title)
            }
            .buttonStyle(.glassProminent)

            Button {
                model.skip(by: NowPlayingCenter.skipInterval)
            } label: {
                Image(systemName: "goforward.15")
            }
            .buttonStyle(.glass)
        }
    }

    private var trackMenu: some View {
        Menu {
            if !model.snapshot.audioTracks.isEmpty {
                Section("Audio") {
                    ForEach(model.snapshot.audioTracks) { track in
                        Button {
                            model.selectAudioTrack(id: track.id)
                        } label: {
                            Label(
                                track.label,
                                systemImage: track.id == model.snapshot.selectedAudioTrackID
                                    ? "checkmark" : ""
                            )
                        }
                        // A track with no decoder on this device stays visible
                        // and disabled: hiding it would look like the file did
                        // not have it.
                        .disabled(!track.isPlayable)
                    }
                }
            }
            Section("Subtitles") {
                Button("Off") { model.selectSubtitleTrack(id: nil) }
                ForEach(model.snapshot.subtitleTracks) { track in
                    Button {
                        model.selectSubtitleTrack(id: track.id)
                    } label: {
                        Label(
                            track.label,
                            systemImage: track.id == model.snapshot.selectedSubtitleTrackID
                                ? "checkmark" : ""
                        )
                    }
                    .disabled(!track.isPlayable)
                }
            }
            Section {
                Text("Playing with \(engineName)")
            }
        } label: {
            Label("Options", systemImage: "captions.bubble")
                .labelStyle(.iconOnly)
        }
        .buttonStyle(.glass)
    }

    private func subtitleOverlay(_ text: String) -> some View {
        VStack {
            Spacer()
            Text(text)
                .font(.title3)
                .multilineTextAlignment(.center)
                .padding(.horizontal, BeamTheme.Spacing.compact)
                .padding(.vertical, BeamTheme.Spacing.small)
                // A solid plate rather than glass: a subtitle has to stay
                // readable over whatever is behind it, and a translucent
                // material is the one place that guarantee fails.
                .background(.black.opacity(0.6), in: RoundedRectangle(cornerRadius: 8))
                .foregroundStyle(.white)
                .padding(.bottom, model.areControlsVisible ? 160 : 60)
        }
        .allowsHitTesting(false)
    }

    private func failure(_ message: String) -> some View {
        VStack(spacing: BeamTheme.Spacing.regular) {
            Image(systemName: "exclamationmark.triangle").font(.largeTitle)
            Text(message).multilineTextAlignment(.center)
            Button("Close", action: onClose).buttonStyle(.glass)
        }
        .foregroundStyle(.white)
        .padding(BeamTheme.Spacing.loose)
        .beamGlassPanel()
        .padding(BeamTheme.Spacing.loose)
    }

    private func upNextCard(_ episode: EpisodeSummary) -> some View {
        VStack(alignment: .trailing) {
            Spacer()
            HStack {
                Spacer()
                VStack(alignment: .leading, spacing: BeamTheme.Spacing.small) {
                    Text("Up next").font(.caption)
                    Text(episode.title).font(.headline).lineLimit(1)
                    Button("Play") { onPlayNext(episode) }
                        .buttonStyle(.glassProminent)
                }
                .beamGlassPadding()
                .beamGlassPanel()
                .frame(maxWidth: 260)
            }
            .padding(BeamTheme.Spacing.loose)
        }
    }

    private var isPlaying: Bool {
        if case .playing = model.snapshot.status { return true }
        return false
    }

    private var engineName: String {
        switch model.engineKind {
        case .avPlayer: "the system player"
        case .sampleBuffer: "Beam's Matroska engine"
        }
    }
}
