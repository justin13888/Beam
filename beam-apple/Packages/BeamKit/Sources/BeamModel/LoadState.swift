import Foundation

/// The lifecycle of one asynchronously loaded value.
///
/// Modelled as an enum rather than as parallel `isLoading`/`value`/`error`
/// properties, because those admit states that cannot happen -- loading and
/// failed at once, a value beside an error -- and every view then has to
/// decide which to believe. Mirrors `LoadState` in `beam-android`'s
/// `core/model`.
public enum LoadState<Value: Sendable>: Sendable {
    /// Nothing has been asked for yet.
    case idle
    /// A request is in flight and nothing has arrived.
    case loading
    /// A value arrived.
    case loaded(Value)
    /// The request failed, with a message fit to show.
    case failed(String)

    /// The loaded value, if there is one.
    public var value: Value? {
        if case .loaded(let value) = self { return value }
        return nil
    }

    /// Whether a request is in flight.
    public var isLoading: Bool {
        if case .loading = self { return true }
        return false
    }

    /// The failure message, if the request failed.
    public var failure: String? {
        if case .failed(let message) = self { return message }
        return nil
    }
}

extension LoadState: Equatable where Value: Equatable {}
