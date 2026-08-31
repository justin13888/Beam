import BeamAppShell
import BeamCore
import SwiftUI

/// The app, on every platform it runs on.
///
/// One entry point for both targets rather than two that drift: the shell
/// already branches where iOS and macOS genuinely differ (a tab bar against a
/// sidebar, a cover against a window), and everything else is identical.
@main
struct BeamApp: App {
    /// The production service graph, built once.
    ///
    /// `@State` rather than a global: the graph owns the one `BeamClient`, and
    /// tying its lifetime to the scene means a preview or a test host gets its
    /// own rather than sharing one that outlives them.
    @State private var services = ServiceContainer.live()

    var body: some Scene {
        WindowGroup {
            BeamRootView(services: services)
        }
        #if os(macOS)
        .defaultSize(width: 1280, height: 820)
        .commands {
            // Replace the New Item command: there is nothing to create,
            // and leaving it would put a menu item there that does nothing.
            CommandGroup(replacing: .newItem) {}
        }
        #endif
    }
}
