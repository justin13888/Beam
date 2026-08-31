import BeamAdmin
import BeamAuth
import BeamCore
import BeamDesignSystem
import BeamDetail
import BeamDownloads
import BeamExplore
import BeamFFI
import BeamHistory
import BeamHome
import BeamLibraries
import BeamModel
import BeamPlayback
import BeamPlayer
import BeamSettings
import BeamUI
import SwiftUI

/// The whole app, from launch to playback.
///
/// This is the one place that knows about every feature, which is what lets
/// each feature depend on none of the others: navigation reaches them as
/// closures passed down from here.
public struct BeamRootView: View {
    @State private var model: AppModel

    /// Build the root over a service graph.
    public init(services: ServiceContainer) {
        _model = State(wrappedValue: AppModel(services: services))
    }

    public var body: some View {
        Group {
            if !model.hasRestored {
                ProgressView().controlSize(.large)
            } else if model.isSignedIn {
                signedIn
            } else {
                AuthScreen(
                    model: AuthModel(servers: model.services.servers),
                    onSignedIn: { model.signedIn() }
                )
            }
        }
        .task { await model.start() }
        .preferredColorScheme(colorScheme)
        .fullScreenCoverCompat(item: $model.player) { presentation in
            player(for: presentation)
        }
    }

    @ViewBuilder
    private var signedIn: some View {
        #if os(macOS)
        // A sidebar rather than a tab bar: macOS 26's sidebar is glass and
        // sits alongside the content, where a tab bar would be a phone
        // idiom transplanted onto a window.
        NavigationSplitView {
            List(TopLevelDestination.allCases, selection: $model.selectedTab) { destination in
                Label(destination.title, systemImage: destination.systemImage)
                    .tag(destination)
            }
            .navigationSplitViewColumnWidth(min: 180, ideal: 220)
        } detail: {
            stack(for: model.selectedTab)
        }
        #else
        TabView(selection: $model.selectedTab) {
            ForEach(TopLevelDestination.allCases) { destination in
                Tab(
                    destination.title,
                    systemImage: destination.systemImage,
                    value: destination,
                    // The search role gives Explore the floating glass
                    // search field the platform provides, rather than a
                    // search bar bolted onto a plain tab.
                    role: destination == .explore ? .search : nil
                ) {
                    stack(for: destination)
                }
            }
        }
        // The tab bar shrinks out of the way as content scrolls up, which
        // is what makes a full-bleed poster grid read as the content and
        // the bar as chrome floating over it.
        .tabBarMinimizeBehavior(.onScrollDown)
        #endif
    }

    @ViewBuilder
    private func stack(for destination: TopLevelDestination) -> some View {
        NavigationStack(path: binding(for: destination)) {
            root(for: destination)
                .navigationDestination(for: Route.self) { route in
                    view(for: route)
                }
        }
    }

    @ViewBuilder
    private func root(for destination: TopLevelDestination) -> some View {
        switch destination {
        case .home:
            HomeScreen(
                model: HomeModel(
                    catalog: model.services.catalog,
                    playback: model.services.playback
                ),
                onOpenTitle: { model.navigate(to: .mediaDetail(mediaId: $0)) },
                onResume: { model.play($0) }
            )
        case .libraries:
            LibrariesScreen(
                model: LibrariesModel(catalog: model.services.catalog),
                onOpenLibrary: { library in
                    model.navigate(
                        to: .libraryDetail(libraryId: library.id, name: library.name)
                    )
                }
            )
        case .explore:
            ExploreScreen(
                model: ExploreModel(catalog: model.services.catalog),
                onOpenTitle: { model.navigate(to: .mediaDetail(mediaId: $0)) }
            )
        case .downloads:
            DownloadsScreen(coordinator: model.downloads) { record in
                model.play(
                    PlaybackRequest(
                        fileId: record.fileId,
                        title: record.title,
                        subtitle: record.subtitle
                    )
                )
            }
        case .settings:
            SettingsScreen(
                model: SettingsModel(
                    preferences: model.services.currentPreferences,
                    servers: model.services.servers,
                    sessions: model.services.sessions,
                    onPreferencesChanged: { model.update(preferences: $0) }
                ),
                onSignedOut: { model.signedOut() },
                onOpenAdmin: { model.navigate(to: .admin) },
                onOpenHistory: { model.navigate(to: .history) }
            )
        }
    }

    @ViewBuilder
    private func view(for route: Route) -> some View {
        switch route {
        case .mediaDetail(let mediaId):
            DetailScreen(
                model: DetailModel(
                    mediaId: mediaId,
                    catalog: model.services.catalog,
                    playback: model.services.playback,
                    quality: model.services.currentPreferences.quality
                ),
                onPlay: { model.play($0) },
                onDownload: { fileId, title in
                    model.downloads.enqueue(
                        fileId: fileId,
                        title: title,
                        subtitle: nil,
                        sizeBytes: nil
                    )
                }
            )
        case .libraryDetail(let libraryId, let name):
            // A library is the catalogue filtered, not a screen of its own --
            // the same reuse `beam-android` makes of its Explore screen.
            ExploreScreen(
                model: ExploreModel(catalog: model.services.catalog),
                title: name,
                onOpenTitle: { model.navigate(to: .mediaDetail(mediaId: $0)) }
            )
            .id(libraryId)
        case .history:
            HistoryScreen(
                model: HistoryModel(playback: model.services.playback),
                onOpenTitle: { model.navigate(to: .mediaDetail(mediaId: $0)) },
                onResume: { model.play($0) }
            )
        case .admin:
            AdminScreen(
                model: AdminModel(
                    admin: model.services.admin,
                    catalog: model.services.catalog
                )
            )
        }
    }

    @ViewBuilder
    private func player(for presentation: PlayerPresentation) -> some View {
        if let context = model.playbackContext(for: presentation.request, container: nil) {
            PlayerScreen(
                model: PlayerModel(
                    request: presentation.request,
                    engine: model.makeEngine(kind: context.kind),
                    engineKind: context.kind,
                    playback: model.services.playback,
                    catalog: model.services.catalog,
                    autoplayNextEpisode: model.services.currentPreferences.autoplayNextEpisode
                ),
                item: context.item,
                onClose: { model.player = nil },
                onPlayNext: { episode in
                    guard let fileId = episode.fileId else { return }
                    model.play(
                        PlaybackRequest(
                            fileId: fileId,
                            mediaId: presentation.request.mediaId,
                            episodeId: episode.id,
                            title: presentation.request.title,
                            subtitle: "Episode \(episode.episodeNumber) - \(episode.title)"
                        )
                    )
                }
            )
        } else {
            BeamStateView(
                .failed(
                    message: "Beam could not work out how to play this file.",
                    isRetryable: false
                )
            )
        }
    }

    private func binding(for destination: TopLevelDestination) -> Binding<NavigationPath> {
        Binding(
            get: { model.paths[destination] ?? NavigationPath() },
            set: { model.paths[destination] = $0 }
        )
    }

    private var colorScheme: ColorScheme? {
        switch model.services.currentPreferences.theme {
        case .system: nil
        case .light: .light
        case .dark: .dark
        }
    }
}

extension View {
    /// `fullScreenCover` on iOS, a plain sheet on macOS.
    ///
    /// `fullScreenCover` does not exist on macOS, and a player that filled the
    /// whole screen would be wrong there anyway -- a window is the macOS idiom.
    @ViewBuilder
    fileprivate func fullScreenCoverCompat<Item: Identifiable, Content: View>(
        item: Binding<Item?>,
        @ViewBuilder content: @escaping (Item) -> Content
    ) -> some View {
        #if os(macOS)
        sheet(item: item, content: content)
        #else
        fullScreenCover(item: item, content: content)
        #endif
    }
}
