import Foundation

/// User settings and the dictionary, persisted in the App Group container.
///
/// Deliberately much smaller than the Mac's `Settings`: this build is cloud-only, so
/// every knob about local models, engines and fallbacks is absent rather than
/// present-and-ignored. Anything worth a switch here is worth one on the Mac too.
@Observable
final class Settings {
    static let shared = Settings()

    private let defaults = Handoff.defaults ?? .standard

    private enum Key {
        static let level = "settings.cleanupLevel"
        static let dictionary = "settings.dictionary"
        static let backgroundSession = "settings.backgroundSession"
        static let appearance = "settings.appearance"
    }

    // MARK: - Appearance

    /// Light, dark, or whatever the system is doing. Default is the system.
    var appearance: Appearance {
        didSet { defaults.set(appearance.rawValue, forKey: Key.appearance) }
    }

    /// The same value, readable without building a `Settings`.
    ///
    /// The keyboard needs the appearance and nothing else from here, and it is a
    /// separate process that cannot ask the app — it reads the shared container
    /// directly. Instantiating the observable object in an extension to get one enum
    /// would drag the dictionary and its JSON along for no reason.
    static var storedAppearance: Appearance {
        let raw = (Handoff.defaults ?? .standard).string(forKey: Key.appearance) ?? ""
        return Appearance(rawValue: raw) ?? .system
    }

    // MARK: - Cleanup level

    var level: CleanupLevel {
        didSet { defaults.set(level.rawValue, forKey: Key.level) }
    }

    // MARK: - Dictionary

    /// The authoritative spellings and their known mishears.
    ///
    /// Stored as the core's own JSON shape so it crosses the FFI unchanged and can be
    /// copied verbatim to and from the Mac's `dictionary.json`.
    var dictionary: DictionaryStore {
        didSet {
            let json = try? JSONSerialization.data(withJSONObject: dictionary.payload)
            defaults.set(json, forKey: Key.dictionary)
        }
    }

    // MARK: - Background session

    /// Whether the app holds a live capture session so the mic key works without a
    /// visible app switch.
    ///
    /// A setting rather than always-on because it is the one feature here that iOS
    /// may take away without saying so: the app can be suspended or killed, and the
    /// honest fallback is opening it. Off means always bounce, which is slower and
    /// completely reliable.
    var keepSessionAlive: Bool {
        didSet { defaults.set(keepSessionAlive, forKey: Key.backgroundSession) }
    }

    private init() {
        level = CleanupLevel(rawValue: defaults.string(forKey: Key.level) ?? "") ?? .light
        keepSessionAlive = defaults.object(forKey: Key.backgroundSession) as? Bool ?? true
        appearance = Appearance(rawValue: defaults.string(forKey: Key.appearance) ?? "") ?? .system
        if let data = defaults.data(forKey: Key.dictionary),
           let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           let entries = object["entries"] as? [[String: Any]] {
            dictionary = DictionaryStore(entries: entries)
        } else {
            dictionary = DictionaryStore()
        }
    }

    // MARK: - Endpoints

    /// Mirrors `whimpr_core::settings`. Kept as literals rather than read over the
    /// bridge because they are needed before any request is built, and a wrong URL
    /// fails loudly on the first call rather than silently.
    enum Groq {
        static let base = "https://api.groq.com/openai/v1"
        static let transcriptions = "\(base)/audio/transcriptions"
        static let chatCompletions = "\(base)/chat/completions"
        static let asrModel = "whisper-large-v3-turbo"
        static let cleanupModel = "openai/gpt-oss-120b"
    }
}

/// The API key, in the Keychain and nowhere else.
///
/// Never in `UserDefaults`, never in the App Group, never in a file — and the
/// keyboard extension never needs it, because the app makes every network call. That
/// is not only tidier: a keyboard is the one process on the device that sees every
/// keystroke, and it has no business holding a credential.
enum APIKey {
    private static let service = "com.whimpr.whimprflow"
    private static let account = "groq.api.key"

    static func load() -> String? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var item: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &item) == errSecSuccess,
              let data = item as? Data,
              let key = String(data: data, encoding: .utf8),
              !key.isEmpty
        else { return nil }
        return key
    }

    /// Why a save did not happen. Surfaced rather than swallowed: a silently failed
    /// save leaves the UI insisting there is no key while the user is sure they just
    /// entered one, with nothing to go on.
    enum SaveError: LocalizedError {
        /// The Keychain refused for want of an `application-identifier` entitlement,
        /// which means the build is unsigned. Expected in an unsigned simulator
        /// build; on a device it means provisioning is wrong.
        case missingEntitlement
        case keychain(OSStatus)

        var errorDescription: String? {
            switch self {
            case .missingEntitlement:
                return "the Keychain is unavailable in an unsigned build"
            case let .keychain(status):
                return "Keychain error \(status)"
            }
        }
    }

    /// Store (or replace) the key. Passing an empty string removes it.
    static func save(_ key: String) -> Result<Void, SaveError> {
        let trimmed = key.trimmingCharacters(in: .whitespacesAndNewlines)
        let base: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
        // Delete first rather than SecItemUpdate: an update against a missing item
        // fails, and branching on that is more code than simply replacing.
        SecItemDelete(base as CFDictionary)
        guard !trimmed.isEmpty else { return .success(()) }

        var add = base
        add[kSecValueData as String] = Data(trimmed.utf8)
        // The key is needed while dictating, which can happen from a locked-screen
        // keyboard; `AfterFirstUnlock` is the loosest setting that still keeps it
        // out of a backup restored onto another device.
        add[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly

        let status = SecItemAdd(add as CFDictionary, nil)
        switch status {
        case errSecSuccess: return .success(())
        // -34018. The build carries no entitlements at all, so there is no keychain
        // access group to write into.
        case errSecMissingEntitlement: return .failure(.missingEntitlement)
        default: return .failure(.keychain(status))
        }
    }

    static var isSet: Bool { load() != nil }
}
