import BeamFFI
import BeamModel
import Foundation

/// The five places the tab bar and the sidebar lead.
///
/// Mirrors `beam-android`'s `TopLevel`, so the two clients present the same
/// structure and a change to one is visibly a change to the other.
public enum TopLevelDestination: String, CaseIterable, Identifiable, Hashable, Sendable {
    /// Continue watching, recently added, top rated.
    case home
    /// The library list.
    case libraries
    /// Search and filter the whole catalogue.
    case explore
    /// Offline downloads.
    case downloads
    /// Preferences and account.
    case settings

    /// Stable identity.
    public var id: String { rawValue }

    /// What to show in the tab bar and sidebar.
    public var title: String {
        switch self {
        case .home: "Home"
        case .libraries: "Libraries"
        case .explore: "Explore"
        case .downloads: "Downloads"
        case .settings: "Settings"
        }
    }

    /// The SF Symbol for the tab.
    public var systemImage: String {
        switch self {
        case .home: "house"
        case .libraries: "square.stack"
        case .explore: "magnifyingglass"
        case .downloads: "arrow.down.circle"
        case .settings: "gearshape"
        }
    }
}

/// Everything that can be pushed onto a navigation stack.
///
/// Typed values rather than string paths, so a navigation call cannot lose or
/// misspell an argument -- the property `beam-android` gets from Navigation 3's
/// typed keys.
public enum Route: Hashable, Sendable {
    /// One title's page.
    case mediaDetail(mediaId: String)
    /// One library's contents.
    case libraryDetail(libraryId: String, name: String)
    /// The watch history.
    case history
    /// The operator screen.
    case admin
}

/// What the player is currently showing, if anything.
///
/// Kept beside the navigation stack rather than in it: the player is presented
/// over whatever is on screen and dismissing it should return there, not pop a
/// level of history.
public struct PlayerPresentation: Identifiable, Equatable {
    /// Stable identity for the presentation.
    public let id = UUID()
    /// What to play.
    public let request: PlaybackRequest

    /// Present `request`.
    public init(request: PlaybackRequest) {
        self.request = request
    }

    public static func == (lhs: PlayerPresentation, rhs: PlayerPresentation) -> Bool {
        lhs.id == rhs.id
    }
}
