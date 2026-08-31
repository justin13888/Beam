import BeamCoreBindings
import Foundation
import os

/// The platform half of the core's persistence boundary.
///
/// The split between plaintext and secret is not decoration: the core exposes
/// them as separate methods precisely so a session cookie cannot be routed
/// into plaintext storage by accident, and this honours that by giving the two
/// halves different backing stores rather than one store and a flag.
///
/// Plaintext -- the server registry, the progress retry queue -- goes to a
/// JSON file in Application Support. Secrets go to the Keychain. Mirrors
/// `DataStoreKeyValueStore.kt` plus `SecretCipher.kt`, except that Apple needs
/// no hand-rolled cipher: the Keychain already holds the key material in the
/// Secure Enclave-backed store that `SecretCipher` reaches for on Android.
public final class KeychainKeyValueStore: KeyValueStore, @unchecked Sendable {
    private let service: String
    private let fileURL: URL
    private let lock = OSAllocatedUnfairLock(initialState: [String: String]())

    /// Create a store writing under `service`.
    ///
    /// - Parameters:
    ///   - service: the Keychain service name, and the plaintext file's stem.
    ///   - directory: where the plaintext file lives. Defaults to Application
    ///     Support, which is excluded from iCloud backup for caches but not
    ///     for this -- the server registry is worth restoring to a new device,
    ///     and the credential it refers to deliberately is not.
    public init(service: String = "net.justinchung.beam", directory: URL? = nil) {
        self.service = service
        let base =
            directory
            ?? FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)
            .first
            ?? FileManager.default.temporaryDirectory
        try? FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
        self.fileURL = base.appendingPathComponent("\(service).plain.json")
        lock.withLock { $0 = Self.readFile(at: fileURL) }
    }

    // MARK: - Plaintext

    public func get(key: String) async throws -> String? {
        lock.withLock { $0[key] }
    }

    public func put(key: String, value: String) async throws {
        try writePlain { $0[key] = value }
    }

    public func remove(key: String) async throws {
        try writePlain { $0.removeValue(forKey: key) }
    }

    public func listKeys(prefix: String) async throws -> [String] {
        lock.withLock { Array($0.keys.filter { $0.hasPrefix(prefix) }) }
    }

    // MARK: - Secrets

    public func getSecret(key: String) async throws -> String? {
        var query = baseQuery(for: key)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne

        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        switch status {
        case errSecSuccess:
            guard let data = item as? Data, let value = String(data: data, encoding: .utf8) else {
                // A value that is present but undecodable is treated as absent
                // rather than as a failure, so a store written by an older
                // build reads as "signed out" instead of bricking the app.
                return nil
            }
            return value
        case errSecItemNotFound:
            return nil
        default:
            throw StorageError.Unavailable(detail: "keychain read failed (\(status))")
        }
    }

    public func putSecret(key: String, value: String) async throws {
        let data = Data(value.utf8)
        let query = baseQuery(for: key)

        let update = [kSecValueData as String: data] as CFDictionary
        let updateStatus = SecItemUpdate(query as CFDictionary, update)
        if updateStatus == errSecSuccess { return }
        guard updateStatus == errSecItemNotFound else {
            throw StorageError.Denied(detail: "keychain update failed (\(updateStatus))")
        }

        var insert = query
        insert[kSecValueData as String] = data
        // AfterFirstUnlock, not WhenUnlocked: playback continues with the
        // screen locked, and the progress queue has to be able to flush there.
        insert[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlock
        let addStatus = SecItemAdd(insert as CFDictionary, nil)
        guard addStatus == errSecSuccess else {
            throw StorageError.Denied(detail: "keychain write failed (\(addStatus))")
        }
    }

    public func removeSecret(key: String) async throws {
        let status = SecItemDelete(baseQuery(for: key) as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw StorageError.Denied(detail: "keychain delete failed (\(status))")
        }
    }

    // MARK: - Internals

    private func baseQuery(for key: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: key,
            // The data-protection keychain, so macOS behaves like iOS rather
            // than falling back to the file-based keychain with its own
            // prompts and its own access-control model.
            kSecUseDataProtectionKeychain as String: true,
        ]
    }

    private func writePlain(_ mutate: @Sendable (inout [String: String]) -> Void) throws {
        let snapshot: [String: String] = lock.withLock { values in
            mutate(&values)
            return values
        }
        do {
            let data = try JSONEncoder().encode(snapshot)
            try data.write(to: fileURL, options: .atomic)
        } catch {
            throw StorageError.Unavailable(detail: error.localizedDescription)
        }
    }

    private static func readFile(at url: URL) -> [String: String] {
        guard let data = try? Data(contentsOf: url),
            let values = try? JSONDecoder().decode([String: String].self, from: data)
        else {
            return [:]
        }
        return values
    }
}
