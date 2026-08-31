import BeamDesignSystem
import BeamFFI
import BeamTesting
import BeamUI
import SnapshotTesting
import SwiftUI
import XCTest

@testable import BeamDetail

/// Reference images for the components every screen is built from.
///
/// The Apple counterpart of `beam-android`'s Roborazzi tier, and the same
/// bargain: pixels are the only way to catch a layout that silently stopped
/// rendering, and they are only meaningful against one fixed device. The
/// simulator and OS are pinned by `mise run apple:snapshot`; a diff is a
/// prompt to look, and then either to fix the view or to re-record with
/// `apple:snapshot:record`.
///
/// Components rather than whole screens. A screen snapshot changes whenever
/// any of its parts does, so it reports a diff without saying what moved --
/// and a suite that always has a diff is a suite nobody reads.
///
/// See ``assertReference(_:file:testName:line:)`` for why these are light-mode
/// only.
@MainActor
final class SnapshotTests: XCTestCase {
    /// The marker `apple:snapshot:record` writes, and nothing else does.
    ///
    /// A file rather than an environment variable. A SwiftPM package test runs
    /// with no test host, so `xcodebuild` treats `TEST_RUNNER_BEAM_SNAPSHOT_RECORD`
    /// as a build setting and never puts it in the test process's environment
    /// -- which made the record task report success while changing nothing, so
    /// an intended visual change could redden CI with no documented way to fix
    /// it. The test bundle already reaches the source tree through `#filePath`,
    /// so a marker beside this file is reachable where an environment variable
    /// is not.
    private static var recordMarker: URL {
        URL(filePath: #filePath).deletingLastPathComponent().appending(path: ".record")
    }

    /// Whether this run should rewrite the references.
    private static var isRecordingRun: Bool {
        FileManager.default.fileExists(atPath: recordMarker.path)
    }

    #if os(iOS)
    func testBadges() {
        let view = HStack(spacing: 8) {
            BeamBadge("1080p")
            BeamBadge("HEVC")
            BeamBadge("HDR10", systemImage: "sun.max", emphasis: .positive)
            BeamBadge("Software", systemImage: "cpu", emphasis: .caution)
            BeamBadge("Unplayable", emphasis: .unavailable)
        }
        .padding()
        .frame(width: 390, height: 80)

        assertReference(view)
    }

    func testStateViews() {
        let view = VStack(spacing: 24) {
            BeamStateView(
                .empty(
                    title: "Nothing found",
                    message: "Try a different search or clear the filters.",
                    systemImage: "magnifyingglass"
                )
            )
            BeamStateView(.failed(message: "Could not reach the server.", isRetryable: true)) {}
        }
        .frame(width: 390, height: 600)

        assertReference(view)
    }

    func testMediaCard() {
        let view = HStack(spacing: 16) {
            MediaCard(
                title: "The Third Man",
                subtitle: "1949",
                artworkURL: nil
            )
            MediaCard(
                title: "A title long enough to wrap onto a second line",
                subtitle: "2024",
                artworkURL: nil,
                progress: 0.42
            )
        }
        .frame(width: 390, height: 320)
        .padding()

        assertReference(view)
    }

    func testSourcePicker() {
        // The case worth a reference image: a playable source beside one
        // this device cannot decode, both visible, with the reason on the
        // second. A regression that hid the rejected source would be
        // invisible to every other test in the suite.
        let playable = Fixtures.source(fileId: "file-1")
        let unplayable = Fixtures.source(
            fileId: "file-2",
            container: "avi",
            videoCodec: "vc1",
            width: 720,
            height: 480
        )
        let view = SourcePicker(
            sources: [playable, unplayable],
            selection: Fixtures.selection(
                source: playable,
                rejected: [Fixtures.rejection(fileId: "file-2")]
            ),
            chosenFileId: nil,
            onChoose: { _ in }
        )
        .padding()
        .frame(width: 390, height: 420)

        assertReference(view)
    }

    /// Record or check `view`.
    ///
    /// **Light only, deliberately.** A dark reference cannot currently be
    /// trusted: `glassEffect` does not render its material offscreen, and
    /// content inside a glass container resolves `Color.primary` against
    /// the light appearance regardless of the host's
    /// `overrideUserInterfaceStyle` or the `\.colorScheme` environment.
    /// A recorded dark image is therefore black text on a black ground --
    /// a picture of a renderer limitation, not of the design, which would
    /// then pass forever whether or not dark mode actually worked.
    ///
    /// Recording it anyway would be worse than not having it. The gap is
    /// listed in `docs/testing.md` beside the other things the hermetic
    /// tier cannot reach, and dark mode is checked on a device.
    private func assertReference(
        _ view: some View,
        file: StaticString = #filePath,
        testName: String = #function,
        line: UInt = #line
    ) {
        let controller = UIHostingController(rootView: view)
        controller.overrideUserInterfaceStyle = .light
        controller.view.backgroundColor = .systemBackground

        // `.missing` on a normal run, so a new reference is written once and an
        // existing one is compared; `.all` only when the record marker says so.
        // `isRecording` would do the same job and is deprecated in 1.19.
        withSnapshotTesting(record: Self.isRecordingRun ? .all : .missing) {
            assertSnapshot(
                of: controller,
                as: .image(on: .iPhone13Pro),
                file: file,
                testName: testName,
                line: line
            )
        }
    }

    #endif
}
