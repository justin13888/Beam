import BeamDesignSystem
import SwiftUI

/// What a screen shows while it has nothing to show.
///
/// Three states, one component, because the alternative is every screen
/// inventing its own spinner and its own empty message -- and then the retry
/// button existing on some failures and not others.
public struct BeamStateView: View {
    /// Which state to render.
    public enum Kind: Equatable, Sendable {
        /// Working.
        case loading
        /// Nothing to show, with an explanation.
        case empty(title: String, message: String, systemImage: String)
        /// Something went wrong.
        case failed(message: String, isRetryable: Bool)
    }

    private let kind: Kind
    private let retry: (() -> Void)?

    /// Render `kind`.
    ///
    /// - Parameter retry: offered only for a failure the core called
    ///   retryable. A retry button on a permanent failure trains people to
    ///   press it and be disappointed.
    public init(_ kind: Kind, retry: (() -> Void)? = nil) {
        self.kind = kind
        self.retry = retry
    }

    public var body: some View {
        switch kind {
        case .loading:
            ProgressView()
                .controlSize(.large)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .accessibilityLabel("Loading")

        case .empty(let title, let message, let systemImage):
            ContentUnavailableView(title, systemImage: systemImage, description: Text(message))

        case .failed(let message, let isRetryable):
            ContentUnavailableView {
                Label("Something went wrong", systemImage: "exclamationmark.triangle")
            } description: {
                Text(message)
            } actions: {
                if isRetryable, let retry {
                    Button("Try again", action: retry)
                        .buttonStyle(.glass)
                }
            }
        }
    }
}
